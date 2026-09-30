use super::*;

#[test]
fn drops_blank_line_between_chinese_sentences() {
    // 线上实测的典型漏网：前段铺陈 + 空行 + 后段收尾。
    let raw =
        "它不知道。是我每次路过都往里看一眼，看有没有你上次翻过的那本。\n\n就当它替我记着了。";
    assert_eq!(
        flatten(raw),
        "它不知道。是我每次路过都往里看一眼，看有没有你上次翻过的那本。就当它替我记着了。"
    );
}

#[test]
fn collapses_every_run_of_whitespace() {
    assert_eq!(flatten("a  \t\n\n  b"), "a b");
    assert_eq!(flatten("a\n\n\n\n\nb"), "a b");
}

#[test]
fn trims_leading_and_trailing_whitespace() {
    assert_eq!(flatten("\n\n  你好  \n\n"), "你好");
    assert_eq!(flatten("   "), "");
    assert_eq!(flatten(""), "");
}

#[test]
fn keeps_single_space_between_latin_words() {
    // 英文断句必须留空格，否则会粘成一个词。
    assert_eq!(flatten("I think.\n\nSo do I."), "I think. So do I.");
    assert_eq!(flatten("hello   world"), "hello world");
}

#[test]
fn mixed_cjk_and_latin_keeps_readable_spacing() {
    // 任一侧是 CJK 就不加空格：中文句子之间加空格是错的。
    assert_eq!(
        flatten("好的。\n\nEnglish follows"),
        "好的。English follows"
    );
    assert_eq!(flatten("wait\n\n等一下"), "wait等一下");
}

#[test]
fn single_newline_between_cjk_is_also_removed() {
    // 不只 \n\n，单个 \n 同样收敛——目标是单段，不是"少一个空行"。
    assert_eq!(flatten("第一句\n第二句"), "第一句第二句");
}

#[test]
fn streaming_matches_one_shot() {
    let raw = "它不知道。\n\n就当它替我记着了。\n\nI remember that book.";
    let mut f = ParagraphFlattener::new();
    let mut streamed = String::new();
    for ch in raw.chars() {
        streamed.push_str(&f.push(&ch.to_string()));
    }
    // finish() 返回完整累积结果（已含各次 push 的产出），不是"剩余尾巴"。
    assert_eq!(f.finish(), flatten(raw));
    // 逐块下发的内容拼接后应与 finish() 一致：用户看到的即入库的。
    assert_eq!(streamed, flatten(raw));
    assert!(!streamed.contains('\n'));
}

#[test]
fn streaming_never_emits_blank_line_mid_stream() {
    // 关键不变式：任何一次 push 的产出都不含换行，且不会产出"段首缩进"。
    let raw = "前面一句。\n\n\n后面一句。\n\n第三句。";
    let mut f = ParagraphFlattener::new();
    for ch in raw.chars() {
        let piece = f.push(&ch.to_string());
        assert!(!piece.contains('\n'), "增量里漏出换行：{piece:?}");
    }
    assert_eq!(f.finish(), "前面一句。后面一句。第三句。");
}

#[test]
fn streaming_splits_the_blank_line_across_chunks() {
    // 换行常被拆到两个 chunk（"\n" 和 "\n"），必须跨 chunk 判定。
    let mut f = ParagraphFlattener::new();
    assert_eq!(f.push("前面一句。"), "前面一句。");
    assert_eq!(f.push("\n"), "", "悬挂空白不下发");
    assert_eq!(f.push("\n"), "", "悬挂空白不下发");
    assert_eq!(f.push("后面一句。"), "后面一句。");
    assert_eq!(f.finish(), "前面一句。后面一句。");
}

#[test]
fn overwrite_replaces_accumulated_text() {
    // End 事件的 captured_texts 才是权威全文，chunk 可能漏收。
    let mut f = ParagraphFlattener::new();
    let _ = f.push("漏收了一部分");
    f.overwrite("完整的第一段。\n\n完整的第二段。");
    assert_eq!(f.finish(), "完整的第一段。完整的第二段。");
}

#[test]
fn overwrite_then_push_continues_cleanly() {
    let mut f = ParagraphFlattener::new();
    f.overwrite("已有内容。");
    assert_eq!(f.push("\n\n再接一句"), "再接一句");
    assert_eq!(f.finish(), "已有内容。再接一句");
}

#[test]
fn preserves_all_non_whitespace_characters() {
    // 归一化不得丢内容：这是它与硬截断的本质区别。
    let raw = "第一段内容，保持原样。\n\n第二段 :: 符号 #1 @user 100% \"引号\" （括号）……";
    let out = flatten(raw);
    for ch in raw.chars().filter(|c| !c.is_whitespace()) {
        assert!(out.contains(ch), "丢失了字符 {ch:?}");
    }
    assert!(out.contains("100%"));
    assert!(out.contains("……"));
}
