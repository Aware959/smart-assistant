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

/// MCP 带回来的成品媒体：投影只说"发了什么"，不把字节写进历史。
#[test]
fn 成品媒体投影只描述形态() {
    let image = Reply::Media {
        bytes: vec![0x89, 0x50],
        mime: "image/png".to_string(),
        filename: Some("pic.png".to_string()),
        caption: None,
    };
    assert_eq!(image.projection(), "[发了一张图片]");
    assert!(image.as_text().is_none());

    let audio = Reply::Media {
        bytes: vec![0x4f, 0x67],
        mime: "audio/ogg".to_string(),
        filename: None,
        caption: None,
    };
    assert_eq!(audio.projection(), "[发了一段音频]");

    let file = Reply::Media {
        bytes: vec![0x50, 0x44, 0x46],
        mime: "application/pdf".to_string(),
        filename: None,
        caption: None,
    };
    assert_eq!(file.projection(), "[发了一个文件]");
}

/// MIME 判不出来的一律当文件：通用投递总能用，猜错成图片反而可能投不出去。
#[test]
fn 媒体种类按mime判定() {
    assert_eq!(MediaKind::from_mime("image/png"), MediaKind::Image);
    assert_eq!(MediaKind::from_mime("IMAGE/JPEG"), MediaKind::Image);
    assert_eq!(MediaKind::from_mime("audio/mpeg"), MediaKind::Audio);
    assert_eq!(MediaKind::from_mime("application/pdf"), MediaKind::File);
    assert_eq!(MediaKind::from_mime(""), MediaKind::File);
}

/// 造一个装在内存库上的 Agent（不碰磁盘库、不连模型）。
fn agent_with(conv: Arc<Sessions>) -> Agent {
    let db = Arc::new(std::sync::Mutex::new(
        crate::db::Database::in_memory().unwrap(),
    ));
    Agent::new(
        conv,
        Arc::new(crate::llm::chat::Llm),
        Arc::new(crate::core::memory::Store::new(db.clone())),
        Arc::new(crate::core::identity::profile::Profile::new(db)),
        crate::core::sticker::StickerCatalog::load_default(),
    )
}

/// 确认闸门：用户点头才放行，拒绝则回填拒绝原因。
///
/// 闸门等的是**会话时间线**里的用户消息——通道侧收到消息的当下就落库，
/// 这一轮跑着的时候新消息也只落库不起第二轮。故这里直接往时间线里写。
#[test]
fn 确认闸门按用户回应放行或拒绝() {
    for (reply, approved) in [("确认", true), ("取消", false)] {
        let conv = Arc::new(sessions());
        let sid = conv.resolve("telegram", "100").unwrap();
        let trigger = conv.add_message(&sid, "user", "帮我跑个工作流").unwrap();
        let agent = agent_with(conv.clone());
        let call = ChatCall::new(
            "c1",
            "mcp__comfy__run_workflow",
            serde_json::json!({"prompt": "一只猫"}),
        );
        let target = crate::mcp::Target {
            server: "comfy".to_string(),
            tool: "run_workflow".to_string(),
        };
        let mut state = DispatchState {
            cursor: Some(trigger.id.clone()),
            confirmed: HashMap::new(),
        };
        let mut delivered: Vec<Reply> = Vec::new();
        // 用户在闸门等到之前回了话。
        conv.add_message(&sid, "user", reply).unwrap();

        let outcome = agent
            .confirm_gate(
                &sid,
                &call,
                &target,
                &mut |_reply: &Reply| Ok(None),
                &mut delivered,
                &mut state,
            )
            .unwrap();

        // 确认请求一定投给了用户（用户得看得到，才谈得上确认）。
        assert_eq!(delivered.len(), 1, "确认请求应投给用户");
        let request = delivered[0].as_text().unwrap_or_default();
        assert!(request.contains("comfy"));
        assert!(request.contains("run_workflow"));

        match (approved, outcome) {
            (true, GateOutcome::Proceed { .. }) => {}
            (false, GateOutcome::Refused { content, .. }) => {
                assert!(content.contains("拒绝"), "回填给模型的说明：{content}");
                assert!(content.contains("未执行"));
            }
            (true, other) => panic!("应放行，却是 {other:?}"),
            (false, other) => panic!("应拒绝，却是 {other:?}"),
        }
    }
}

/// 同一批里完全相同的调用只问一次：第二次沿用上一次的判定。
#[test]
fn 同一批里重复的调用不重复问用户() {
    let conv = Arc::new(sessions());
    let sid = conv.resolve("telegram", "101").unwrap();
    let trigger = conv.add_message(&sid, "user", "跑一下").unwrap();
    let agent = agent_with(conv.clone());
    let call = ChatCall::new(
        "c1",
        "mcp__comfy__run_workflow",
        serde_json::json!({"prompt": "猫"}),
    );
    let target = crate::mcp::Target {
        server: "comfy".to_string(),
        tool: "run_workflow".to_string(),
    };
    let mut state = DispatchState {
        cursor: Some(trigger.id.clone()),
        confirmed: HashMap::new(),
    };
    let mut delivered: Vec<Reply> = Vec::new();
    conv.add_message(&sid, "user", "取消").unwrap();

    let first = agent
        .confirm_gate(
            &sid,
            &call,
            &target,
            &mut |_reply: &Reply| Ok(None),
            &mut delivered,
            &mut state,
        )
        .unwrap();
    assert!(matches!(first, GateOutcome::Refused { .. }));

    // 第二次同样的调用：不再投递确认请求，也不等新的回应。
    let mut delivered2: Vec<Reply> = Vec::new();
    let second = agent
        .confirm_gate(
            &sid,
            &call,
            &target,
            &mut |_reply: &Reply| Ok(None),
            &mut delivered2,
            &mut state,
        )
        .unwrap();
    assert!(delivered2.is_empty(), "不该再问一次");
    assert!(matches!(second, GateOutcome::Refused { .. }));
}

/// 确认请求投递不出去 = 用户没看到 = 不能当作已同意。
#[test]
fn 确认请求投递失败即视为未确认() {
    let conv = Arc::new(sessions());
    let sid = conv.resolve("telegram", "102").unwrap();
    let trigger = conv.add_message(&sid, "user", "跑一下").unwrap();
    let agent = agent_with(conv.clone());
    let call = ChatCall::new("c1", "mcp__comfy__run_workflow", serde_json::json!({}));
    let target = crate::mcp::Target {
        server: "comfy".to_string(),
        tool: "run_workflow".to_string(),
    };
    let mut state = DispatchState {
        cursor: Some(trigger.id.clone()),
        confirmed: HashMap::new(),
    };
    let mut delivered: Vec<Reply> = Vec::new();

    let outcome = agent
        .confirm_gate(
            &sid,
            &call,
            &target,
            &mut |_reply: &Reply| Err("通道断了".to_string()),
            &mut delivered,
            &mut state,
        )
        .unwrap();
    assert!(delivered.is_empty());
    match outcome {
        GateOutcome::Refused { content, .. } => {
            assert!(content.contains("未执行"), "回填给模型的说明：{content}");
        }
        other => panic!("应拒绝，却是 {other:?}"),
    }
}
