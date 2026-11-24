use super::*;

fn msg(text: Option<&str>, sticker_emoji: Option<Option<&str>>) -> TgMessage {
    TgMessage {
        text: text.map(str::to_string),
        sticker: sticker_emoji.map(|e| TgSticker {
            emoji: e.map(str::to_string),
        }),
        voice: None,
        audio: None,
        chat: TgChat { id: 1 },
        date: None,
    }
}

/// 造一条语音条消息。
fn voice_msg() -> TgMessage {
    TgMessage {
        voice: Some(TgVoice {}),
        ..msg(None, None)
    }
}

#[test]
fn 文本消息原样取出() {
    assert_eq!(
        inbound_text(&msg(Some("在的"), None)).as_deref(),
        Some("在的")
    );
}

#[test]
fn 空文本且无贴纸返回none() {
    assert_eq!(inbound_text(&msg(Some("   "), None)), None);
    assert_eq!(inbound_text(&msg(None, None)), None);
}

#[test]
fn 贴纸消息转成可读占位文本() {
    assert_eq!(
        inbound_text(&msg(None, Some(Some("😀")))).as_deref(),
        Some("[对方发来一张表情包 😀]")
    );
}

/// 贴纸没有 emoji 标签（少见但存在）也要能收到，不能整条丢掉。
#[test]
fn 无emoji的贴纸也能收到() {
    assert_eq!(
        inbound_text(&msg(None, Some(None))).as_deref(),
        Some("[对方发来一张表情包]")
    );
}

/// 同时带文字与贴纸时以文字为准（贴纸只是附件，不覆盖用户真正说的话）。
#[test]
fn 文本优先于贴纸() {
    assert_eq!(
        inbound_text(&msg(Some("哈哈"), Some(Some("😀")))).as_deref(),
        Some("哈哈")
    );
}

/// `sendDice` 响应里 `result.dice.value` 就是服务端定好的点数，必须原样取回。
#[test]
fn 骰子响应取出点数() {
    let result = Some(serde_json::json!({ "dice": { "value": 4, "emoji": "🎲" } }));
    assert_eq!(dice_value(&result), Ok(4));
}

/// 缺字段 / 非数字都不能瞎编，一律当失败上抛（点数由服务端决定，编一个等于骗模型）。
#[test]
fn 骰子响应缺字段报错() {
    assert!(dice_value(&None).is_err());
    assert!(dice_value(&Some(serde_json::json!({}))).is_err());
    assert!(dice_value(&Some(serde_json::json!({ "dice": {} }))).is_err());
    assert!(dice_value(&Some(serde_json::json!({ "dice": { "value": "4" } }))).is_err());
}

/// 越界值（非 1–6）同样拒绝：宁可退回通用确认，也不把非法点数交给模型。
#[test]
fn 骰子响应越界报错() {
    assert!(dice_value(&Some(serde_json::json!({ "dice": { "value": 0 } }))).is_err());
    assert!(dice_value(&Some(serde_json::json!({ "dice": { "value": 7 } }))).is_err());
}

/// 旁白上限按 UTF-16 码元算：1024 个 emoji 已经占满 2048，必须判超限。
#[test]
fn 旁白上限按码元计() {
    assert!(caption_fits(&"a".repeat(MAX_CAPTION_LEN)));
    assert!(!caption_fits(&"a".repeat(MAX_CAPTION_LEN + 1)));
    // 每个 emoji 占 2 个码元：512 个刚好，513 个超限。
    assert!(caption_fits(&"😀".repeat(MAX_CAPTION_LEN / 2)));
    assert!(!caption_fits(&"😀".repeat(MAX_CAPTION_LEN / 2 + 1)));
}

/// 失败轮次的重试开关：一条都没送达才重试（用户什么都没看到），有送达就不整轮重跑。
#[test]
fn 有一条送达就不再整轮重试() {
    assert!(should_retry_after_failure(0));
    assert!(!should_retry_after_failure(1));
    assert!(!should_retry_after_failure(3));
}

/// 投递计数只认成功：失败片段不算"用户看到过"，否则该重试的轮次会被白白放弃。
#[test]
fn 投递计数只统计成功送达的片段() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Delivery>();
    let deliverer = TgDeliver::new(tx);
    // 假工人：第一条回成功、第二条回失败，然后收工。
    let worker = std::thread::spawn(move || {
        let first = rx.blocking_recv().expect("第一条投递");
        first.ack.send(Ok(None)).unwrap();
        let second = rx.blocking_recv().expect("第二条投递");
        second.ack.send(Err("发送失败".to_string())).unwrap();
    });

    assert_eq!(deliverer.delivered_count(), 0);
    assert_eq!(deliverer.deliver(&Reply::Text("在".to_string())), Ok(None));
    assert_eq!(deliverer.delivered_count(), 1);
    assert!(deliverer.deliver(&Reply::Text("哦".to_string())).is_err());
    assert_eq!(deliverer.delivered_count(), 1);

    worker.join().unwrap();
}

/// 投递口已关（工人退出）时上抛错误且不计数：调用方据此走失败分支，而不是以为送达了。
#[test]
fn 投递通道关闭时报错且不计数() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Delivery>();
    let deliverer = TgDeliver::new(tx);
    drop(rx);

    assert!(deliverer.deliver(&Reply::Text("在".to_string())).is_err());
    assert_eq!(deliverer.delivered_count(), 0);
}

// ---------------------------------------------------------------------------
// 入站语音转写
// ---------------------------------------------------------------------------

/// 造一条音频**文件**消息。
fn audio_msg() -> TgMessage {
    TgMessage {
        audio: Some(TgAudio {}),
        ..msg(None, None)
    }
}

/// 解析 Telegram 真实形状的语音条报文：听写能力已下线，只认得出"这条是语音"。
#[test]
fn 解析语音条报文() {
    let raw = r#"{
        "message_id": 7,
        "from": {"id": 1},
        "chat": {"id": 42},
        "date": 1700000000,
        "voice": {"file_id": "AwACAg", "duration": 3, "mime_type": "audio/ogg", "file_size": 9293}
    }"#;
    let m: TgMessage = serde_json::from_str(raw).expect("语音条报文应能解析");
    assert_eq!(m.chat.id, 42);
    assert_eq!(m.date, Some(1700000000));
    assert!(m.has_audio(), "voice 应被识别成音频");
    assert!(matches!(m.resolve_inbound(), Some(Inbound::Audio)));
}

/// 音频文件走 `audio` 字段，同样只认得出在不在。
#[test]
fn 解析音频文件报文() {
    let raw = r#"{
        "chat": {"id": 42},
        "caption": "昨天会议的录音",
        "audio": {"file_id": "BQACAg", "duration": 90, "file_name": "meeting.m4a", "title": "周会"}
    }"#;
    let m: TgMessage = serde_json::from_str(raw).expect("音频文件报文应能解析");
    assert!(m.has_audio(), "audio 应被识别成音频");
    assert!(matches!(m.resolve_inbound(), Some(Inbound::Audio)));
}

/// 只有 `file_id` 的音频（不能假设字段给全）：其余字段缺失不该让整条消息解析失败。
#[test]
fn 音频字段缺失时仍能识别() {
    let raw = r#"{"chat": {"id": 1}, "voice": {"file_id": "X"}}"#;
    let m: TgMessage = serde_json::from_str(raw).expect("字段缺失不该让整条消息解析失败");
    assert!(m.has_audio());
}

/// 内容来源的优先级：文本 > 贴纸 > 语音 > 音频。文本与语音同时在时以文本为准——
/// 用户打的字是他真正的意思，语音只是附带。
#[test]
fn 内容来源优先级为文本贴纸语音音频() {
    assert!(matches!(
        msg(Some("喂"), None).resolve_inbound(),
        Some(Inbound::Text(t)) if t == "喂"
    ));
    assert!(matches!(
        msg(None, Some(Some("😀"))).resolve_inbound(),
        Some(Inbound::Text(_))
    ));
    assert!(matches!(
        voice_msg().resolve_inbound(),
        Some(Inbound::Audio)
    ));
    assert!(matches!(
        audio_msg().resolve_inbound(),
        Some(Inbound::Audio)
    ));

    let both = TgMessage {
        text: Some("顺便说一句".to_string()),
        ..voice_msg()
    };
    assert!(matches!(both.resolve_inbound(), Some(Inbound::Text(t)) if t == "顺便说一句"));
}

/// 照片/视频等我们没建模的消息类型仍然是"整条忽略"（改动前的行为）。
#[test]
fn 未建模的消息类型仍然忽略() {
    let raw = r#"{"chat": {"id": 1}, "photo": [{"file_id": "p", "width": 1, "height": 1}]}"#;
    let m: TgMessage = serde_json::from_str(raw).expect("未知字段应被忽略而不是报错");
    assert!(m.resolve_inbound().is_none());
}

// ---------------------------------------------------------------------------

#[test]
fn 解析身份管理命令并区分普通消息() {
    assert_eq!(parse_command("/identity"), Ok(Some(Command::Identity)));
    assert_eq!(parse_command("/proposals"), Ok(Some(Command::Proposals)));
    assert_eq!(
        parse_command("/approve abc consent"),
        Ok(Some(Command::Approve {
            id: "abc".to_string(),
            consent: true
        }))
    );
    assert_eq!(
        parse_command("/approve abc --consent"),
        Ok(Some(Command::Approve {
            id: "abc".to_string(),
            consent: true
        }))
    );
    assert_eq!(
        parse_command("/approve abc"),
        Ok(Some(Command::Approve {
            id: "abc".to_string(),
            consent: false
        }))
    );
    assert_eq!(
        parse_command("/reject abc"),
        Ok(Some(Command::Reject {
            id: "abc".to_string()
        }))
    );
    assert_eq!(
        parse_command("/rollback"),
        Ok(Some(Command::Rollback { version: None }))
    );
    assert_eq!(
        parse_command("/rollback 7"),
        Ok(Some(Command::Rollback { version: Some(7) }))
    );
    assert_eq!(parse_command("/version"), Ok(Some(Command::Version)));
    assert_eq!(
        parse_command("/identity@my_bot"),
        Ok(Some(Command::Identity)),
        "Telegram 群组命令带 bot 后缀"
    );
    assert_eq!(parse_command("今天吃什么"), Ok(None));
}

#[test]
fn 非法管理命令给出用法而不进入对话() {
    assert!(parse_command("/approve").unwrap_err().contains("用法"));
    assert!(parse_command("/reject").unwrap_err().contains("用法"));
    assert!(parse_command("/rollback abc").unwrap_err().contains("用法"));
    assert_eq!(parse_command("/unknown"), Ok(Some(Command::Help)));
}

#[test]
fn 身份与提案格式化包含审批所需信息() {
    let view = IdentityView {
        version: Some(2),
        text: "性格：慢热；价值观：真诚".to_string(),
    };
    let text = format_identity(&view);
    assert!(text.contains("v2"));
    assert!(text.contains("性格：慢热"));

    let proposal = ProposalView {
        id: "p1".to_string(),
        field_key: "relationship_stance".to_string(),
        before_value: Some("保持距离".to_string()),
        after_value: "更亲近但尊重独处".to_string(),
        reason: "用户表达了希望更亲近".to_string(),
        evidence_message_ids: vec!["m1".to_string()],
        confidence: "high".to_string(),
        risk_level: "high".to_string(),
        status: "pending".to_string(),
        proposer: "identity-evolution-v1".to_string(),
        decided_by: None,
        base_version: 1,
    };
    let text = format_proposal(&proposal);
    assert!(text.contains("p1"));
    assert!(text.contains("关系姿态"));
    assert!(text.contains("保持距离 → 更亲近但尊重独处"));
    assert!(text.contains("m1"));
}

#[test]
fn 空提案列表有明确回执() {
    assert_eq!(format_proposals(&[]), "当前没有待审批的身份提案。");
}

#[test]
fn 非白名单管理员不能查看或修改身份() {
    for command in [
        Command::Identity,
        Command::Proposals,
        Command::Approve {
            id: "p1".to_string(),
            consent: false,
        },
        Command::Reject {
            id: "p1".to_string(),
        },
        Command::Rollback { version: None },
    ] {
        assert!(
            is_admin_command(&command),
            "身份管理动作都必须经过管理员门禁"
        );
    }
    for command in [
        Command::Start,
        Command::New,
        Command::Version,
        Command::Help,
    ] {
        assert!(!is_admin_command(&command));
    }
    assert!(admin_denied().contains("只允许白名单管理员"));
}
