//! `history.rs` 的工具往返相关单元测试：压缩时的工具调用组对齐、读时的配对净化。

use super::*;
use crate::core::session::MessageTurn;

/// 一条纯文本轮次（时间戳无意义，压缩只按下标）。
fn plain(role: &str, content: &str) -> MessageTurn {
    MessageTurn {
        id: String::new(),
        role: role.to_string(),
        content: content.to_string(),
        created_at: String::new(),
        tool_calls: None,
        tool_call_id: None,
        tool_name: None,
    }
}

/// 一条带工具调用声明的 assistant 轮。
fn calls(content: &str, call_ids: &[&str]) -> MessageTurn {
    let calls: Vec<ChatCall> = call_ids
        .iter()
        .map(|id| ChatCall::new(*id, "send_message", serde_json::json!({})))
        .collect();
    MessageTurn {
        tool_calls: Some(serde_json::to_string(&calls).unwrap()),
        ..plain("assistant", content)
    }
}

/// 一条工具结果行。
fn result(call_id: &str) -> MessageTurn {
    MessageTurn {
        tool_call_id: Some(call_id.to_string()),
        tool_name: Some("send_message".to_string()),
        ..plain("tool", crate::llm::tool::SEND_MESSAGE_ACK)
    }
}

/// 组装一条内存消息（净化函数的输入）。
fn chat(role: &str, content: &str, calls: &[&str], tool_call_id: Option<&str>) -> ChatMessage {
    ChatMessage {
        tool_calls: calls
            .iter()
            .map(|id| ChatCall::new(*id, "send_message", serde_json::json!({})))
            .collect(),
        tool_call_id: tool_call_id.map(String::from),
        ..ChatMessage::new(role, content)
    }
}

/// 断言序列里不存在残缺配对：每个调用都有结果，每条结果都有声明它的调用。
fn assert_pairs_complete(messages: &[ChatMessage]) {
    for (idx, msg) in messages.iter().enumerate() {
        if msg.role == "assistant" {
            for call in &msg.tool_calls {
                assert!(
                    messages[idx + 1..].iter().any(|m| m.role == "tool"
                        && m.tool_call_id.as_deref() == Some(call.call_id.as_str())),
                    "调用 {} 没有配对的结果行",
                    call.call_id
                );
            }
        }
        if msg.role == "tool" {
            let id = msg.tool_call_id.as_deref().expect("结果行必须有 call_id");
            assert!(
                messages[..idx]
                    .iter()
                    .any(|m| m.tool_calls.iter().any(|c| c.call_id == id)),
                "结果行 {id} 找不到声明它的调用"
            );
        }
    }
}

/// 压缩的裁剪切口落在工具调用组中间时，整组必须一起留下。
#[test]
fn compaction_keeps_tool_groups_whole() {
    let total = DAY_CONTEXT_MAX + 10;
    let tail_start = total - (DAY_CONTEXT_MAX - DAY_KEEP_HEAD);
    let mut turns: Vec<MessageTurn> = (0..total)
        .map(|i| plain("user", &format!("消息{i}")))
        .collect();

    // 头边界组：assistant 声明在 DAY_KEEP_HEAD - 2，结果行跨进省略段。
    let head = DAY_KEEP_HEAD - 2;
    turns[head] = calls("说话A", &["a1", "a2"]);
    turns[head + 1] = result("a1");
    turns[head + 2] = result("a2");

    // 尾边界组：声明落在省略段内，结果行跨进保留的尾段。
    let tail = tail_start - 2;
    turns[tail] = calls("说话B", &["b1", "b2"]);
    turns[tail + 1] = result("b1");
    turns[tail + 2] = result("b2");

    let out = build_day_history(&turns).unwrap();

    assert_pairs_complete(&out);
    // 两组都是"组内有一条本会被保留"，因此整组留下。
    for marker in ["说话A", "说话B"] {
        assert!(
            out.iter().any(|m| m.content == marker),
            "整组被误裁：{marker}"
        );
    }
    for id in ["a1", "a2", "b1", "b2"] {
        assert!(
            out.iter()
                .any(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(id)),
            "结果行 {id} 被裁掉"
        );
    }
}

/// 组内一条都不在保留窗口里时，整组一起丢弃（不许留下孤立的声明或结果行）。
#[test]
fn compaction_drops_tool_groups_whole() {
    let total = DAY_CONTEXT_MAX + 10;
    let tail_start = total - (DAY_CONTEXT_MAX - DAY_KEEP_HEAD);
    let mut turns: Vec<MessageTurn> = (0..total)
        .map(|i| plain("user", &format!("消息{i}")))
        .collect();

    // 整组都落在省略段正中。
    let mid = (DAY_KEEP_HEAD + tail_start) / 2;
    turns[mid] = calls("被省略的说话", &["c1"]);
    turns[mid + 1] = result("c1");

    let out = build_day_history(&turns).unwrap();

    assert_pairs_complete(&out);
    assert!(!out.iter().any(|m| m.content == "被省略的说话"));
    assert!(!out
        .iter()
        .any(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("c1")));
}

/// 完整配对正常通过净化，不被改动。
#[test]
fn sanitize_keeps_complete_pairs() {
    let msgs = vec![
        chat("assistant", "", &["c1", "c2"], None),
        chat("tool", "已发送", &[], Some("c1")),
        chat("tool", "已发送", &[], Some("c2")),
        chat("assistant", "说完了", &[], None),
    ];
    let out = sanitize_tool_pairs(msgs.clone());
    assert_eq!(out.len(), msgs.len());
    assert_eq!(out[0].tool_calls.len(), 2);
    assert_pairs_complete(&out);
}

/// 结果行找不到声明它的调用 → 丢弃（"有结果没调用"同样是 provider 会拒的结构）。
#[test]
fn sanitize_drops_orphan_tool_result() {
    let msgs = vec![
        chat("user", "在吗", &[], None),
        chat("tool", "已发送", &[], Some("c9")),
        chat("assistant", "在的", &[], None),
    ];
    let out = sanitize_tool_pairs(msgs);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|m| m.role != "tool"));
}

/// 调用只配到部分结果 → 只保留有结果的声明（残留的调用声明会被 provider 拒）。
#[test]
fn sanitize_keeps_only_answered_calls() {
    let msgs = vec![
        chat("assistant", "", &["c1", "c2", "c3"], None),
        chat("tool", "已发送", &[], Some("c1")),
    ];
    let out = sanitize_tool_pairs(msgs);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].tool_calls.len(), 1);
    assert_eq!(out[0].tool_calls[0].call_id, "c1");
    assert_pairs_complete(&out);
}

/// 一个都没配到：有正文就退化成纯 assistant 轮，没正文就整轮丢掉。
#[test]
fn sanitize_degrades_unanswered_assistant_round() {
    let with_text = sanitize_tool_pairs(vec![
        chat("assistant", "我本想发两句", &["c1", "c2"], None),
        chat("user", "嗯", &[], None),
    ]);
    assert_eq!(with_text.len(), 2);
    assert!(with_text[0].tool_calls.is_empty());
    assert_eq!(with_text[0].content, "我本想发两句");

    let without_text = sanitize_tool_pairs(vec![
        chat("assistant", "", &["c1"], None),
        chat("user", "嗯", &[], None),
    ]);
    // 空正文的孤立声明轮没有留下的意义，直接消失。
    assert_eq!(without_text.len(), 1);
    assert_eq!(without_text[0].role, "user");
}

/// 净化对已经合法的序列是幂等的（正常路径不该被改动）。
#[test]
fn sanitize_is_idempotent_on_complete_pairs() {
    let msgs = vec![
        chat("assistant", "", &["c1"], None),
        chat("tool", crate::llm::tool::SEND_MESSAGE_ACK, &[], Some("c1")),
    ];
    let once = sanitize_tool_pairs(msgs);
    let twice = sanitize_tool_pairs(once.clone());
    assert_eq!(once.len(), twice.len());
    assert_eq!(twice[0].tool_calls.len(), 1);
}
