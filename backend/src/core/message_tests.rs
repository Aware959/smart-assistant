use super::*;

#[test]
fn assistant_calls_保留工具参数原文() {
    let m = ChatMessage::assistant_calls(
        "我先看看几点",
        vec![ChatCall::new("call_1", "get_now", serde_json::json!({}))],
    );
    assert_eq!(m.role, "assistant");
    assert_eq!(m.content, "我先看看几点");
    assert_eq!(m.tool_calls.len(), 1);
    assert_eq!(m.tool_calls[0].fn_name, "get_now");
    // 流式里 arguments 是逐片到达的半截 JSON，genai 在 End 才累积成完整对象。
    assert_eq!(m.tool_calls[0].fn_arguments, serde_json::json!({}));
    assert_eq!(m.tool_calls[0].call_id, "call_1");
}

#[test]
fn tool_result_带调用名与配对id() {
    let call = ChatCall::new("call_9", "get_now", serde_json::json!({}));
    let m = ChatMessage::tool_result(&call, "14:30");
    assert_eq!(m.role, "tool");
    assert_eq!(m.tool_call_id.as_deref(), Some("call_9"));
    assert_eq!(m.tool_name.as_deref(), Some("get_now"));
    assert_eq!(m.content, "14:30");
}

#[test]
fn arg_str_只取字符串参数() {
    let call = ChatCall::new(
        "c",
        "send_message",
        serde_json::json!({ "text": "在的", "n": 3 }),
    );
    assert_eq!(call.arg_str("text"), Some("在的"));
    // 类型不符 / 字段缺失都必须返回 None，不能 panic。
    assert_eq!(call.arg_str("n"), None);
    assert_eq!(call.arg_str("missing"), None);
}

#[test]
fn 普通消息不携带工具字段() {
    let m = ChatMessage::assistant("在的");
    assert!(m.tool_calls.is_empty());
    assert!(m.tool_call_id.is_none());
    assert!(m.tool_name.is_none());
}
