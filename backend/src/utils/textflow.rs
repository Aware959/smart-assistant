//! 回复文本的空白归一化（把模型偶发的段落分隔收敛成单段）。
//!
//! # 为什么需要它
//!
//! 提示词已经在表达"单段、别换行"，但提示词永远是概率约束：DeepSeek-V4.1-Flash
//! 这类蒸馏小模型在实测里仍有约 16% 的回复带 `\n\n`（典型是"前段铺陈 + 空行 +
//! 后段收尾"的抒情排版）。提示词负责让这个比例降下来，这里负责把漏网的那部分
//! 变成**确定性**结果——两者是叠加关系，不是二选一。
//!
//! # 定位：归一化，不是塑形
//!
//! 本模块只处理**空白**：把连续空白收敛成一个分隔符、去掉首尾空白。
//! 它不截断、不删词、不改写，因此不会产生"半句话"——这与曾经的
//! `MAX_REPLY_CHARS` 硬截断有本质区别，后者才是被砍掉的那个。
//!
//! # 为什么必须在流里做
//!
//! 不能等 `complete_stream` 拿到完整文本再归一化：那时 `on_delta` 早就把带
//! `\n\n` 的原文推给用户了（SSE / WebSocket / CLI 都是逐段渲染的），事后改写只会
//! 让"看到的"和"入库的"不一致。所以本模块是一个**流式状态机**：
//!
//! - 收到空白 → 暂存，不下发（此时还不知道后面跟的是句中空格还是段落分隔）；
//! - 收到下一个非空白字符 → 此时才能判定，产出该下发的片段；
//! - 流结束 → 丢弃悬挂的尾部空白。
//!
//! 代价是首字节之后可能有几十毫秒的额外延迟（要等下一个非空白字符才敢下发），
//! 相对于 LLM 首字延迟可以忽略。

/// 流式空白归一化器：把逐段到达的模型输出收敛成单段文本。
///
/// 语义（与整体 `flatten` 一致，只是分块喂入）：
/// - 连续空白（空格 / 制表 / 换行，含 `\n\n`）收敛成**至多一个**分隔符；
/// - 文本首部的空白直接丢弃；
/// - 文本尾部的空白在 [`Self::finish`] 时丢弃；
/// - 分隔符两侧只要有一边是 CJK 文字或中文标点，就**不留空格**（中文句子之间
///   加空格是错的）；两边都是拉丁字符时保留单个空格（英文断句需要）。
#[derive(Debug, Default)]
pub struct ParagraphFlattener {
    /// 完整输出（已定稿的部分）。
    out: String,
    /// 最近一个已下发的非空白字符，用于判定分隔符是否需要。
    last: Option<char>,
    /// 已收到但尚未判定的空白。
    pending_ws: bool,
    /// 是否已下发过非空白字符（决定首部空白是丢弃还是保留）。
    started: bool,
}

impl ParagraphFlattener {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一个增量，返回**本次应当下发给宿主**的片段（可能是空串）。
    ///
    /// 片段末尾不会残留未判定的空白：那些要等下一次调用才知道该不该变成空格。
    pub fn push(&mut self, chunk: &str) -> String {
        let mut emit = String::with_capacity(chunk.len());
        for ch in chunk.chars() {
            if ch.is_whitespace() {
                self.pending_ws = true;
                continue;
            }
            if self.started && self.pending_ws && self.needs_space(ch) {
                emit.push(' ');
                self.out.push(' ');
            }
            self.pending_ws = false;
            emit.push(ch);
            self.out.push(ch);
            self.last = Some(ch);
            self.started = true;
        }
        emit
    }

    /// 收尾：丢弃悬挂的尾部空白，返回最终完整文本。
    pub fn finish(&mut self) -> &str {
        self.pending_ws = false;
        &self.out
    }

    /// 是否还没产出过任何内容（用于日志判定"首字节到达"）。
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    /// 用权威全文覆盖已累积的结果（流式 chunk 可能漏收，`End` 事件里的
    /// `captured_texts` 才是完整文本）。
    ///
    /// 只纠正最终入库的文本，**不重复下发增量**——用户已经看到的流不回滚。
    pub fn overwrite(&mut self, full: &str) {
        self.out = flatten(full);
        self.last = self.out.chars().next_back();
        self.pending_ws = false;
        self.started = !self.out.is_empty();
    }

    /// 悬挂空白之后紧跟 `next` 时，是否需要补一个空格。
    fn needs_space(&self, next: char) -> bool {
        match self.last {
            // 中文语境（占绝大多数）不留空格；纯拉丁文才需要空格断句。
            Some(prev) => !is_cjk(prev) && !is_cjk(next),
            // 文本开头：前面的空白直接丢弃。
            None => false,
        }
    }
}

/// 一次性归一化整段文本（与 [`ParagraphFlattener`] 语义一致）。
pub fn flatten(text: &str) -> String {
    let mut f = ParagraphFlattener::new();
    f.push(text);
    f.finish().to_string()
}

/// 是否为 CJK 表意文字或中文标点：这些字符两侧不需要空格分隔。
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303F      // 中日韩符号和标点：。，、；：！？「」『』…—
        | 0x3400..=0x4DBF    // 扩展 A（罕用字）
        | 0x4E00..=0x9FFF    // 基本区（常用汉字）
        | 0xF900..=0xFAFF    // 兼容表意文字
        | 0xFF00..=0xFFEF    // 全角字符（含全角字母数字）
    )
}

#[cfg(test)]
#[path = "textflow_tests.rs"]
mod textflow_tests;
