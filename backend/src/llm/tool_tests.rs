use super::*;

/// 记录写入调用的桩：把 remember 的入参抄下来，才能断言派发确实传对了。
struct Stub {
    now: String,
    recall: String,
    writes: std::cell::RefCell<Vec<(String, MemoryKind, Option<String>)>>,
    /// remember 的返回值（`Err` 文案即模拟写入失败）。
    write_result: std::result::Result<WriteKind, String>,
    /// 贴纸目录：决定 `send_sticker` 认哪些键。
    stickers: StickerCatalog,
}

impl ToolContext for Stub {
    fn now(&self) -> String {
        self.now.clone()
    }
    fn recall(&self, _query: &str) -> String {
        self.recall.clone()
    }
    fn remember(
        &self,
        content: &str,
        kind: MemoryKind,
        supersedes: Option<&str>,
    ) -> std::result::Result<WriteKind, String> {
        self.writes
            .borrow_mut()
            .push((content.to_string(), kind, supersedes.map(str::to_string)));
        self.write_result.clone()
    }
    fn sticker_keys(&self) -> Vec<String> {
        self.stickers.keys()
    }
    fn sticker_file_id(&self, key: &str) -> Option<String> {
        self.stickers.file_id(key).map(str::to_string)
    }
}

fn stub() -> Stub {
    Stub {
        now: "2026-10-04 20:15".to_string(),
        recall: "· 喜欢手冲".to_string(),
        writes: std::cell::RefCell::new(Vec::new()),
        write_result: Ok(WriteKind::Created),
        stickers: StickerCatalog::empty(),
    }
}

/// 带一张贴纸的桩：键 `😀` → 固定 `file_id`，够验证派发链路。
fn sticker_stub() -> Stub {
    let mut stickers = StickerCatalog::empty();
    stickers.set = "TestPack".to_string();
    stickers
        .stickers
        .insert("😀".to_string(), "CAAC-test".to_string());
    Stub { stickers, ..stub() }
}

fn call(name: &str, args: serde_json::Value) -> ChatCall {
    ChatCall::new("call_1", name, args)
}

#[test]
fn send_message_投递归一化后的正文() {
    let out = dispatch(
        &call(SEND_MESSAGE, serde_json::json!({ "text": "在的" })),
        &stub(),
    );
    assert!(matches!(out, ToolOutcome::Sent(t) if t == "在的"));
}

#[test]
fn send_message_空白折叠后为空则不投递() {
    // 归一化后为空 = 没有真话可说，落到 Filled 让模型知道这轮白说了，
    // 不能当成一条空消息投给用户。
    let out = dispatch(
        &call(SEND_MESSAGE, serde_json::json!({ "text": "  \n  " })),
        &stub(),
    );
    assert!(matches!(out, ToolOutcome::Filled(_)));
}

#[test]
fn send_message_超长不投递也不截断() {
    let long = "字".repeat(SEND_MESSAGE_MAX_CHARS + 1);
    let out = dispatch(
        &call(SEND_MESSAGE, serde_json::json!({ "text": long })),
        &stub(),
    );
    match out {
        // 关键是**不能**是 Sent：截断会留下半句话，比长句更像故障。
        ToolOutcome::Sent(_) => panic!("超长消息不应被投递"),
        ToolOutcome::SentDice(_) => panic!("send_message 不应产出骰子"),
        ToolOutcome::SentSticker(_) => panic!("send_message 不应产出贴纸"),
        ToolOutcome::Filled(msg) => assert!(msg.contains("超过单条")),
        ToolOutcome::Rejected(_) => panic!("超长属参数问题，应提示改写而非拒绝"),
    }
}

#[test]
fn send_message_缺参数被拒绝() {
    let out = dispatch(&call(SEND_MESSAGE, serde_json::json!({})), &stub());
    assert!(matches!(out, ToolOutcome::Rejected(_)));
}

#[test]
fn 只读工具只回填不投递() {
    let now = dispatch(&call(GET_NOW, serde_json::json!({})), &stub());
    assert!(matches!(now, ToolOutcome::Filled(t) if t == "2026-10-04 20:15"));

    let recall = dispatch(
        &call(RECALL_MEMORY, serde_json::json!({ "query": "  咖啡  " })),
        &stub(),
    );
    // query 首尾空白要被裁掉，召回质量取决于这个。
    assert!(matches!(recall, ToolOutcome::Filled(t) if t == "· 喜欢手冲"));
}

#[test]
fn recall_query_为空被拒绝() {
    for q in [serde_json::json!({}), serde_json::json!({ "query": "   " })] {
        let out = dispatch(&call(RECALL_MEMORY, q), &stub());
        assert!(matches!(out, ToolOutcome::Rejected(_)));
    }
}

#[test]
fn 未知工具被拒绝而不是panic() {
    let out = dispatch(&call("rm_rf", serde_json::json!({})), &stub());
    assert!(matches!(out, ToolOutcome::Rejected(m) if m.contains("rm_rf")));
}

#[test]
fn remember_写入并回填确认() {
    let ctx = stub();
    let out = dispatch(
        &call(
            REMEMBER,
            serde_json::json!({ "kind": "trait", "content": "  用户现在住在上海  " }),
        ),
        &ctx,
    );

    assert_eq!(out, ToolOutcome::Filled(REMEMBER_ACK_NEW.to_string()));
    let writes = ctx.writes.borrow();
    assert_eq!(writes.len(), 1);
    // 首尾空白必须裁掉：落库的是记忆内容，多余空白会污染相似度比较。
    assert_eq!(writes[0].0, "用户现在住在上海");
    assert_eq!(writes[0].1, MemoryKind::Trait);
    assert_eq!(writes[0].2, None, "未传 supersedes 时不应凭空造一个");
}

/// kind 是必填的：它决定寿命，缺了就只能靠"默认永久"兜底，那正是过去
/// 「下周要考试」被存成永久事实的原因。宁可拒绝，也不能缺省写入。
#[test]
fn remember_缺kind被拒绝() {
    for args in [
        serde_json::json!({ "content": "用户喜欢手冲" }),
        serde_json::json!({ "kind": "  ", "content": "用户喜欢手冲" }),
        serde_json::json!({ "kind": "core", "content": "用户喜欢手冲" }),
        serde_json::json!({ "kind": "记忆", "content": "用户喜欢手冲" }),
    ] {
        let ctx = stub();
        let out = dispatch(&call(REMEMBER, args), &ctx);
        assert!(
            matches!(out, ToolOutcome::Rejected(m) if m.contains("kind")),
            "缺 kind / 非法 kind 应被明确拒绝"
        );
        assert!(ctx.writes.borrow().is_empty(), "被拒绝时不应写入");
    }
}

/// 枚举的四种值都要能被解析到——schema 里列出来的，dispatch 就必须认。
#[test]
fn remember_四个kind都能解析() {
    for (raw, expect) in [
        ("trait", MemoryKind::Trait),
        ("preference", MemoryKind::Preference),
        ("routine", MemoryKind::Routine),
        ("plan", MemoryKind::Plan),
        // 模型偶尔会跟着上文的中文名走，大小写也要兜住。
        ("Routine", MemoryKind::Routine),
    ] {
        let ctx = stub();
        let out = dispatch(
            &call(
                REMEMBER,
                serde_json::json!({ "kind": raw, "content": "用户午休时间是 12 点到 2 点" }),
            ),
            &ctx,
        );
        assert_eq!(out, ToolOutcome::Filled(REMEMBER_ACK_NEW.to_string()));
        assert_eq!(ctx.writes.borrow()[0].1, expect);
    }
}

#[test]
fn remember_取代旧记忆时透传id() {
    let ctx = Stub {
        write_result: Ok(WriteKind::Revised),
        ..stub()
    };
    let out = dispatch(
        &call(
            REMEMBER,
            serde_json::json!({
                "kind": "trait",
                "content": "用户现在住在上海",
                "supersedes": " m-42 ",
            }),
        ),
        &ctx,
    );

    assert_eq!(out, ToolOutcome::Filled(REMEMBER_ACK_REVISED.to_string()));
    assert_eq!(ctx.writes.borrow()[0].2.as_deref(), Some("m-42"));
}

/// 空串 supersedes 应当按"没传"处理：模型偶尔会填个空串，
/// 直接当 id 传下去会让写入报"记忆不存在"，白白浪费一轮。
#[test]
fn remember_空白supersedes按未传处理() {
    let ctx = stub();
    dispatch(
        &call(
            REMEMBER,
            serde_json::json!({ "kind": "preference", "content": "用户喜欢手冲", "supersedes": "   " }),
        ),
        &ctx,
    );
    assert_eq!(ctx.writes.borrow()[0].2, None);
}

#[test]
fn remember_缺内容被拒绝() {
    for args in [
        serde_json::json!({ "kind": "trait" }),
        serde_json::json!({ "kind": "trait", "content": "  " }),
    ] {
        let ctx = stub();
        let out = dispatch(&call(REMEMBER, args), &ctx);
        assert!(matches!(out, ToolOutcome::Rejected(_)));
        assert!(ctx.writes.borrow().is_empty(), "被拒绝时不应写入");
    }
}

#[test]
fn remember_超长被拒绝而不是截断() {
    let ctx = stub();
    let long = "字".repeat(REMEMBER_MAX_CHARS + 1);
    let out = dispatch(
        &call(
            REMEMBER,
            serde_json::json!({ "kind": "trait", "content": long }),
        ),
        &ctx,
    );
    assert!(matches!(out, ToolOutcome::Rejected(m) if m.contains("超过单条")));
    assert!(ctx.writes.borrow().is_empty(), "超长不应写入");
}

/// 写入失败要如实回填，不能伪装成成功——模型据此才能改参数或干脆不记。
#[test]
fn remember_写入失败被拒绝() {
    let ctx = Stub {
        write_result: Err("要取代的记忆不存在：m-99".to_string()),
        ..stub()
    };
    let out = dispatch(
        &call(
            REMEMBER,
            serde_json::json!({
                "kind": "trait",
                "content": "用户现在住在上海",
                "supersedes": "m-99",
            }),
        ),
        &ctx,
    );
    assert!(matches!(out, ToolOutcome::Rejected(m) if m.contains("m-99")));
}

#[test]
fn send_dice_合法emoji投递() {
    let out = dispatch(
        &call(SEND_DICE, serde_json::json!({ "emoji": "🎯" })),
        &stub(),
    );
    assert!(matches!(out, ToolOutcome::SentDice(e) if e == "🎯"));
}

/// emoji 缺省为 🎲：模型只调 `send_dice` 不带参数时也该发得出去。
#[test]
fn send_dice_缺省为骰子() {
    let out = dispatch(&call(SEND_DICE, serde_json::json!({})), &stub());
    assert!(matches!(out, ToolOutcome::SentDice(e) if e == "🎲"));
}

/// 白名单之外的 emoji 必须在本地拒绝，别丢给 Telegram 报错。
#[test]
fn send_dice_非法emoji被拒绝() {
    let out = dispatch(
        &call(SEND_DICE, serde_json::json!({ "emoji": "😀" })),
        &stub(),
    );
    assert!(matches!(out, ToolOutcome::Rejected(m) if m.contains("😀")));
}

#[test]
fn 全部工具的schema都能序列化() {
    for def in definitions(&StickerCatalog::empty()) {
        // serde_json::Value 恒可序列化，这里真正要守的是工具名不重复——
        // 重名会让模型侧的注册表互相覆盖。
        let name = def.name.as_str();
        assert!(!name.is_empty());
        assert_eq!(
            definitions(&StickerCatalog::empty())
                .iter()
                .filter(|d| d.name.as_str() == name)
                .count(),
            1,
            "工具名重复：{name}"
        );
        assert!(def.description.is_some());
        assert!(def.schema.is_some(), "{name} 缺少参数 schema");
    }
    // 没拉过贴纸目录 = 无贴纸工具；骰子不需要素材，永远在。
    assert_eq!(definitions(&StickerCatalog::empty()).len(), 5);
}

/// 有目录时才多出 `send_sticker`，且它的参数枚举就是目录里的键——
/// 模型看得见的可选项必须和 dispatch 认的可选项是同一份。
#[test]
fn 有贴纸目录时下发send_sticker且枚举即目录键() {
    let catalog = {
        let mut c = StickerCatalog::empty();
        c.stickers.insert("😀".into(), "a".into());
        c.stickers.insert("抱抱".into(), "b".into());
        c
    };
    let defs = definitions(&catalog);
    assert_eq!(defs.len(), 6, "有目录时应多出 send_sticker");
    let def = defs
        .into_iter()
        .find(|d| d.name.as_str() == SEND_STICKER)
        .expect("send_sticker 应在工具目录里");
    let schema = serde_json::to_string(def.schema.as_ref().expect("应有 schema")).unwrap();
    assert!(
        schema.contains("😀"),
        "schema 的 key 枚举应列出目录键：{schema}"
    );
    assert!(
        schema.contains("抱抱"),
        "schema 的 key 枚举应列出目录键：{schema}"
    );
    assert!(
        schema.contains(r#""required":["key"]"#),
        "key 应为必填：{schema}"
    );
}

#[test]
fn send_sticker_合法键投递file_id() {
    let out = dispatch(
        &call(SEND_STICKER, serde_json::json!({ "key": "😀" })),
        &sticker_stub(),
    );
    assert_eq!(out, ToolOutcome::SentSticker("CAAC-test".to_string()));
}

/// 键不在目录里必须本地拒绝并回填可选项：让模型一次改对，
/// 而不是拿一个编出来的值去 Telegram 撞服务端错误。
#[test]
fn send_sticker_非法键被拒绝并回填可选项() {
    let out = dispatch(
        &call(SEND_STICKER, serde_json::json!({ "key": "没见过" })),
        &sticker_stub(),
    );
    assert!(
        matches!(out, ToolOutcome::Rejected(ref m) if m.contains("没见过") && m.contains("😀")),
        "拒绝文案应同时点名非法键与可用键：{out:?}"
    );
}

#[test]
fn send_sticker_缺key被拒绝() {
    for args in [serde_json::json!({}), serde_json::json!({ "key": "  " })] {
        let out = dispatch(&call(SEND_STICKER, args), &sticker_stub());
        assert!(matches!(out, ToolOutcome::Rejected(_)));
    }
}

/// 记忆写入的判据必须跟着工具定义一起下发，不能只躺在别处：
/// 少了描述，模型就只能凭感觉决定记什么，记忆库会重新长回什么都收的样子。
#[test]
fn remember_描述里带准入三道闸() {
    let def = definitions(&StickerCatalog::empty())
        .into_iter()
        .find(|d| d.name.as_str() == REMEMBER)
        .expect("remember 应在工具目录里");
    let desc = def.description.clone().unwrap_or_default();
    for gate in ["关于用户本人", "自足", "泛泛而谈"] {
        assert!(desc.contains(gate), "工具描述缺少准入判据 {gate}：{desc}");
    }
    // 四种类的含义和各自的寿命也必须在下发的描述里，不能只存在于 Rust 常量。
    for kind in MemoryKind::ALL {
        assert!(
            desc.contains(kind.as_str()),
            "工具描述缺少种类 {}：{desc}",
            kind.as_str()
        );
    }
    let schema = serde_json::to_string(def.schema.as_ref().expect("remember 应有 schema")).unwrap();
    assert!(schema.contains("supersedes"), "schema 应声明 supersedes");
    assert!(
        schema.contains(r#""kind""#) && schema.contains(r#""required":["kind","content"]"#),
        "schema 应把 kind 列为必填：{schema}"
    );
    for kind in MemoryKind::ALL {
        assert!(
            schema.contains(kind.as_str()),
            "schema 的 kind 枚举应列出 {}：{schema}",
            kind.as_str()
        );
    }
}
