use super::*;

struct Stub {
    now: String,
    recall: String,
}

impl ToolContext for Stub {
    fn now(&self) -> String {
        self.now.clone()
    }
    fn recall(&self, _query: &str) -> String {
        self.recall.clone()
    }
}

fn stub() -> Stub {
    Stub {
        now: "2026-10-04 20:15".to_string(),
        recall: "· 喜欢手冲".to_string(),
    }
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
fn 三个工具的schema都能序列化() {
    for def in definitions() {
        // serde_json::Value 恒可序列化，这里真正要守的是工具名不重复——
        // 重名会让模型侧的注册表互相覆盖。
        let name = def.function.name.clone();
        assert!(!name.is_empty());
        assert_eq!(
            definitions().iter().filter(|d| d.function.name == name).count(),
            1,
            "工具名重复：{name}"
        );
        assert!(def.function.description.is_some());
    }
    assert_eq!(definitions().len(), 3);
}