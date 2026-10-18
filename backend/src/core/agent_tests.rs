//! `agent.rs` 的单元测试：回复片段的文本投影（历史入库与纯文本宿主都吃这一份），
//! 以及"运行中插话"的并入逻辑（[`ingest_steers`]）。

use super::*;

fn sessions() -> Sessions {
    Sessions::new(Arc::new(std::sync::Mutex::new(
        crate::db::Database::in_memory().unwrap(),
    )))
    .unwrap()
}

/// 插话只并**水位线之后的用户消息**，且游标推进后同一条不会并第二遍。
#[test]
fn 插话并入本轮上下文并推进游标() {
    let conv = sessions();
    let sid = conv.resolve("telegram", "100").unwrap();
    let trigger = conv.add_message(&sid, "user", "在吗").unwrap();

    // 本轮 live 上下文：系统提示 + 触发消息（与 `chat_stream` 组装的一致）。
    let mut messages = vec![ChatMessage::system("你是助手"), ChatMessage::user("在吗")];
    let mut cursor = Some(trigger.id.clone());

    // 还没有插话时什么都不并。
    assert_eq!(
        ingest_steers(&conv, &sid, &mut cursor, &mut messages).unwrap(),
        0
    );

    // 本轮自己的往返（assistant / tool 行）不该被当成插话并进来。
    conv.add_message_full(
        &sid,
        "assistant",
        "",
        &crate::db::message::ToolMeta {
            tool_calls: Some(
                serde_json::to_string(&vec![ChatCall::new(
                    "c1",
                    "send_message",
                    serde_json::json!({}),
                )])
                .unwrap(),
            ),
            tool_call_id: None,
            tool_name: None,
        },
    )
    .unwrap();
    conv.add_message(&sid, "assistant", "先说一句").unwrap();
    // 运行中用户又说的两句。
    conv.add_message(&sid, "user", "顺便问下").unwrap();
    conv.add_message(&sid, "user", "还有这个").unwrap();

    assert_eq!(
        ingest_steers(&conv, &sid, &mut cursor, &mut messages).unwrap(),
        2
    );
    let tail: Vec<&str> = messages[2..].iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        tail,
        vec!["顺便问下", "还有这个"],
        "按时间正序并进 live 上下文"
    );
    assert!(
        messages[2..].iter().all(|m| m.role == "user"),
        "只并用户消息"
    );

    // 游标已停在最后一条上：再调不重复并入。
    assert_eq!(
        ingest_steers(&conv, &sid, &mut cursor, &mut messages).unwrap(),
        0
    );
    assert_eq!(messages.len(), 4);
}

/// 游标为 `None`（主动开口轮次）时不并入：那种轮次的语境不该被用户消息打断。
#[test]
fn 无游标时不并入插话() {
    let conv = sessions();
    let sid = conv.resolve("telegram", "100").unwrap();
    conv.add_message(&sid, "user", "新来的话").unwrap();

    let mut messages = vec![ChatMessage::system("你是助手")];
    let mut cursor = None;
    assert_eq!(
        ingest_steers(&conv, &sid, &mut cursor, &mut messages).unwrap(),
        0
    );
    assert_eq!(messages.len(), 1);
    assert_eq!(cursor, None);
}

/// 水位线认不出来（历史被压缩/重置）时什么都不并——绝不把整段历史当插话重灌一遍。
#[test]
fn 水位线失效时不重灌历史() {
    let conv = sessions();
    let sid = conv.resolve("telegram", "100").unwrap();
    conv.add_message(&sid, "user", "旧话一").unwrap();
    conv.add_message(&sid, "user", "旧话二").unwrap();

    let mut messages = vec![ChatMessage::system("你是助手")];
    let mut cursor = Some("不存在的 id".to_string());
    assert_eq!(
        ingest_steers(&conv, &sid, &mut cursor, &mut messages).unwrap(),
        0
    );
    assert_eq!(messages.len(), 1, "历史不得被当成插话灌进来");
    assert_eq!(cursor, Some("不存在的 id".to_string()));
}

/// 语音投影要带上真正念出去的那段话：用户听到了，后续轮次的模型也必须看得到，
/// 否则它会知道自己"发过一条语音"，却不知道说了什么。
#[test]
fn 语音投影带出念的话() {
    let reply = Reply::Voice {
        text: "晚安，早点睡".to_string(),
    };
    assert_eq!(reply.projection(), "[发了一段语音] 晚安，早点睡");
}

/// 图片投影带上生成描述（可选旁白跟在后面）：图片本身进不了历史，
/// 描述是它唯一的文本痕迹。
#[test]
fn 图片投影带出画面描述与旁白() {
    let with_caption = Reply::Photo {
        prompt: "一只橘猫".to_string(),
        caption: Some("看这只猫".to_string()),
    };
    assert_eq!(
        with_caption.projection(),
        "[发了一张图片：一只橘猫] 看这只猫"
    );

    let bare = Reply::Photo {
        prompt: "一只橘猫".to_string(),
        caption: None,
    };
    assert_eq!(bare.projection(), "[发了一张图片：一只橘猫]");
}

/// 贴纸不带选择键：键只是目录里的索引，对"说过什么"没有信息量。
#[test]
fn 贴纸投影不带选择键() {
    let reply = Reply::Sticker {
        file_id: "CAACAgEAAxkBAAI".to_string(),
    };
    assert_eq!(reply.projection(), "[发了一个表情包]");
}
