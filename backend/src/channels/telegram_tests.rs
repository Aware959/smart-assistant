use super::*;

fn msg(text: Option<&str>, sticker_emoji: Option<Option<&str>>) -> TgMessage {
    TgMessage {
        text: text.map(str::to_string),
        sticker: sticker_emoji.map(|e| TgSticker {
            emoji: e.map(str::to_string),
        }),
        voice: None,
        audio: None,
        caption: None,
        chat: TgChat { id: 1 },
        date: None,
    }
}

/// 造一条语音条消息（无 caption，语音条本来也不带）。
fn voice_msg(file_id: &str) -> TgMessage {
    TgMessage {
        voice: Some(TgVoice {
            file_id: file_id.to_string(),
            duration: Some(3),
            mime_type: Some("audio/ogg".to_string()),
            file_size: Some(9293),
            file_name: None,
            title: None,
        }),
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

/// 造一条音频**文件**消息（可带用户自己写的 caption）。
fn audio_msg(file_id: &str, caption: Option<&str>) -> TgMessage {
    TgMessage {
        audio: Some(TgAudio {
            file_id: file_id.to_string(),
            duration: Some(90),
            mime_type: None, // 故意不给，用来验证按类型回退的默认 MIME
            file_size: Some(1_500_000),
            file_name: Some("meeting.m4a".to_string()),
            title: Some("周会".to_string()),
        }),
        caption: caption.map(str::to_string),
        ..msg(None, None)
    }
}

/// 解析 Telegram 真实形状的语音条报文（字段名/嵌套以 Bot API 为准，不是我们编的）。
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
    let a = m.audio_attachment().expect("voice 应被识别成音频附件");
    assert_eq!(a.file_id, "AwACAg");
    assert_eq!(a.duration, Some(3));
    assert_eq!(a.mime(), "audio/ogg");
    assert_eq!(a.file_size, Some(9293));
    assert!(a.is_voice);
}

/// 音频文件走 `audio` 字段，且可以带 `caption`；缺 `mime_type` 时按"它是文件而非语音条"回退。
#[test]
fn 解析音频文件报文与旁白() {
    let raw = r#"{
        "chat": {"id": 42},
        "caption": "昨天会议的录音",
        "audio": {"file_id": "BQACAg", "duration": 90, "file_name": "meeting.m4a", "title": "周会"}
    }"#;
    let m: TgMessage = serde_json::from_str(raw).expect("音频文件报文应能解析");
    assert_eq!(m.caption.as_deref(), Some("昨天会议的录音"));
    let a = m.audio_attachment().expect("audio 应被识别成音频附件");
    assert_eq!(a.mime(), "audio/mpeg");
    assert!(!a.is_voice);
    assert_eq!(a.file_name, Some("meeting.m4a"));
    assert_eq!(a.title, Some("周会"));
}

/// 只带 `file_id` 的音频（Telegram 理论上会给全，但不能假设）：其余字段缺失不该解析失败。
#[test]
fn 音频字段缺失时解析为none() {
    let raw = r#"{"chat": {"id": 1}, "voice": {"file_id": "X"}}"#;
    let m: TgMessage = serde_json::from_str(raw).expect("字段缺失不该让整条消息解析失败");
    let a = m.audio_attachment().expect("voice 应被识别成音频附件");
    assert_eq!(a.file_id, "X");
    assert_eq!(a.duration, None);
    assert_eq!(a.mime_type, None);
    assert_eq!(a.file_size, None);
    assert_eq!(a.mime(), "audio/ogg"); // 语音条缺 mime 时的回退
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
        voice_msg("v").resolve_inbound(),
        Some(Inbound::Audio(_))
    ));
    assert!(matches!(
        audio_msg("a", None).resolve_inbound(),
        Some(Inbound::Audio(_))
    ));

    let both = TgMessage {
        text: Some("顺便说一句".to_string()),
        ..voice_msg("v")
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

/// 分流判据是 HTTP 状态码：4xx 是"这条音频的问题"，5xx / 意外值是我们自己的问题。
#[test]
fn 状态码决定分流() {
    assert_eq!(
        voice_outcome(200, r#"{"text":"今天晚上想吃什么"}"#),
        VoiceOutcome::Transcribed {
            text: "今天晚上想吃什么".to_string()
        }
    );
    // 200 但没听出内容 -> 交模型回应，不是错误。
    assert_eq!(
        voice_outcome(200, r#"{"text":""}"#),
        VoiceOutcome::Unintelligible
    );
    assert_eq!(
        voice_outcome(200, r#"{"text":" 。。? "}"#),
        VoiceOutcome::Unintelligible
    );
    assert_eq!(voice_outcome(422, "{}"), VoiceOutcome::Unintelligible);

    // 内容层面失败：解不开 / 太长 / 不是音频。
    assert!(matches!(
        voice_outcome(400, r#"{"error":"无法解码音频"}"#),
        VoiceOutcome::Rejected { reason } if reason == "无法解码音频"
    ));
    assert!(matches!(
        voice_outcome(413, r#"{"error":"超过 20MB"}"#),
        VoiceOutcome::Rejected { .. }
    ));
    assert!(matches!(
        voice_outcome(415, ""),
        VoiceOutcome::Rejected { .. }
    ));

    // 系统故障：我们自己的问题，绝不伪装成"没听清"。
    assert!(matches!(
        voice_outcome(503, r#"{"kind":"engine_unavailable"}"#),
        VoiceOutcome::Skipped { reason } if reason.contains("engine_unavailable")
    ));
    assert!(matches!(
        voice_outcome(500, ""),
        VoiceOutcome::Skipped { .. }
    ));
    // 意外状态码按"我们自己的问题"处理：不猜，也别把它说成用户音频的错。
    assert!(matches!(
        voice_outcome(418, ""),
        VoiceOutcome::Skipped { .. }
    ));
    // 200 但不是契约约定的形状 = 契约被破坏，同样算我们自己的问题。
    assert!(matches!(
        voice_outcome(200, "not json"),
        VoiceOutcome::Skipped { .. }
    ));
    assert!(matches!(
        voice_outcome(200, r#"{"no":"text"}"#),
        VoiceOutcome::Skipped { .. }
    ));
}

/// "没听出来"的判据是"有没有字母/数字/表意文字"，不是"有多长"：
/// "嗯""好"这类简短应答是正常转写，必须交给模型理解，不能被当成没听清。
#[test]
fn 简短应答算听出来了() {
    assert!(!heard_nothing("嗯"));
    assert!(!heard_nothing("好"));
    assert!(!heard_nothing("ok"));
    assert!(!heard_nothing("行吧"));
    assert!(heard_nothing(""));
    assert!(heard_nothing("   "));
    assert!(heard_nothing("。"));
    assert!(heard_nothing("?!…"));
}

/// 转写成功的内容带"源自语音"前缀，且转写文本首尾空白被去掉。
#[test]
fn 转写成功的内容带来源前缀() {
    assert_eq!(
        transcribed_content(None, "今天晚上想吃什么"),
        "[语音] 今天晚上想吃什么"
    );
    assert_eq!(transcribed_content(None, "  在的  "), "[语音] 在的");
}

/// caption 是用户自己写的说明，必须排在前面且不被转写结果覆盖或丢弃（FR-012）。
#[test]
fn 旁白在前且不被覆盖() {
    assert_eq!(
        transcribed_content(Some("昨天会议的录音"), "讨论了排期"),
        "昨天会议的录音\n[语音] 讨论了排期"
    );
    // 只有空白的 caption 等于没有。
    assert_eq!(transcribed_content(Some("   "), "在的"), "[语音] 在的");
}

/// FR-010：转写文本**只**作为落库内容存在，绝不作为回复发给用户。
///
/// 这条不变量在类型层面也成立——成功路径唯一的产物就是 `transcribed_content` 返回的
/// `String`，它的去向只有 `session.add_message_at`；没有任何分支把它交给 `send_text`。
/// 这里用可断言的形式固定住：产物必然带 `[语音] ` 前缀，即它是**时间线标记**，
/// 而不是可以直接回给用户的一句话。
#[test]
fn 转写产物是时间线标记而非回显文案() {
    let content = transcribed_content(Some("会议录音"), "讨论了排期");
    assert!(content.starts_with("会议录音\n[语音] "));
    assert!(content.ends_with("讨论了排期"));
    assert_eq!(VOICE_PREFIX, "[语音] ");
}

// ---------------------------------------------------------------------------
// 入站语音转写：失败分流（US2）
// ---------------------------------------------------------------------------

/// 假转写调用器：只数调用次数、按预设作答。零网络、零依赖。
///
/// 用 `AtomicUsize` 而非 `Cell`：`Transcriber` 的 future 要求 `Send`，而 `Cell` 不是
/// `Sync`，`&FakeTranscriber` 就过不了 `Send`。
struct FakeTranscriber {
    calls: std::sync::atomic::AtomicUsize,
    reply: Result<(u16, String), String>,
}

impl FakeTranscriber {
    /// 预设一个网关响应（状态码 + 响应体）。
    fn replying(status: u16, body: &str) -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            reply: Ok((status, body.to_string())),
        }
    }

    /// 预设"连下载都没走通"。
    fn unreachable(msg: &str) -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            reply: Err(msg.to_string()),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Transcriber for FakeTranscriber {
    async fn transcribe(
        &self,
        _file_id: &str,
        _mime: &str,
        _timeout: u64,
    ) -> Result<(u16, String), String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.reply.clone()
    }
}

/// 测试用的音频附件视角。
fn audio_ref(duration: Option<i64>, file_size: Option<i64>) -> AudioRef<'static> {
    AudioRef {
        file_id: "AwACAg",
        duration,
        mime_type: Some("audio/ogg"),
        file_size,
        file_name: None,
        title: None,
        is_voice: true,
    }
}

/// 测试用的转写参数（默认值语义：启用、超时 180s、时长上限 120s）。
fn voice_cfg(enabled: bool) -> VoiceCfg {
    VoiceCfg {
        enabled,
        timeout: 180,
        max_secs: 120,
    }
}

/// 前置拦截的两道闸：时长上限与 20 MB 体积上限，**用同一个标记**（对用户与模型来说
/// "太大了"是同一件事）。
#[test]
fn 前置拦截拦下超时长与超体积() {
    let max: u64 = 120;

    // 正常范围 -> 放行。
    assert_eq!(
        voice_plan(&audio_ref(Some(3), Some(9293)), max),
        VoicePlan::Transcribe
    );
    // 边界值本身不算超限（`>` 而非 `>=`）：正好 120s / 正好 20MB 都要放行。
    assert_eq!(
        voice_plan(&audio_ref(Some(max as i64), Some(MAX_VOICE_BYTES)), max),
        VoicePlan::Transcribe
    );

    // 时长超限 -> 拦。
    assert_eq!(
        voice_plan(&audio_ref(Some(max as i64 + 1), Some(9293)), max),
        VoicePlan::Reject(VOICE_TOO_LARGE_MARKER)
    );
    // 体积超限 -> 拦。
    assert_eq!(
        voice_plan(&audio_ref(Some(3), Some(MAX_VOICE_BYTES + 1)), max),
        VoicePlan::Reject(VOICE_TOO_LARGE_MARKER)
    );
    // 两种超限产出**同一个**标记（T021④）。
    assert_eq!(
        voice_plan(&audio_ref(Some(9999), None), max),
        voice_plan(&audio_ref(None, Some(i64::MAX)), max)
    );

    // 字段缺失 -> 不拦：不知道就别猜。
    assert_eq!(
        voice_plan(&audio_ref(None, None), max),
        VoicePlan::Transcribe
    );
}

/// 超限音频必须**在下载之前**被拦下：转写调用器的调用次数必须是 0（FR-003/T018）。
#[tokio::test]
async fn 超限语音不发起下载() {
    let fake = FakeTranscriber::replying(200, r#"{"text":"不该被问到"}"#);
    let long = audio_ref(Some(3600), Some(9293));
    let got = inbound_audio_content(&voice_cfg(true), 1, &long, None, &fake).await;

    assert_eq!(got.as_deref(), Some(VOICE_TOO_LARGE_MARKER));
    assert_eq!(fake.calls(), 0, "超限语音不该触发任何下载/转写调用");

    let big = audio_ref(Some(3), Some(50 * 1024 * 1024));
    let got = inbound_audio_content(&voice_cfg(true), 1, &big, None, &fake).await;
    assert_eq!(got.as_deref(), Some(VOICE_TOO_LARGE_MARKER));
    assert_eq!(fake.calls(), 0, "超体积语音不该触发任何下载/转写调用");
}

/// 内容层面失败（没听清 / 解不开 / 太大）**要落可落库的标记并照常起轮次**——交给模型
/// 自己用自然语言回应，而不是我们编一句（FR-004/005）。
#[tokio::test]
async fn 内容层面失败产出交给模型的情境标记() {
    let cases: &[(&str, (u16, &str))] = &[
        ("空文本", (200, r#"{"text":""}"#)),
        ("只剩标点", (200, r#"{"text":" 。。? "}"#)),
        ("网关说没有有效语音", (422, "{}")),
        ("解不开", (400, r#"{"error":"无法解码音频"}"#)),
        ("太大", (413, r#"{"error":"超过上限"}"#)),
        ("格式不支持", (415, r#"{"error":"未知编码"}"#)),
    ];
    for (name, (status, body)) in cases {
        let fake = FakeTranscriber::replying(*status, body);
        let got =
            inbound_audio_content(&voice_cfg(true), 1, &audio_ref(Some(3), None), None, &fake)
                .await;
        assert_eq!(
            got.as_deref(),
            Some(VOICE_UNINTELLIGIBLE_MARKER),
            "{name} 应落情境标记"
        );
        assert_eq!(fake.calls(), 1, "{name} 应真的问过网关一次");
    }
}

/// 系统故障（能力没开 / 连不上 / 超时 / 网关 5xx）**不产生任何可落库文本**——没有标记、
/// 没有回显，`None` 会让调用方直接跳过落库与起轮次（FR-006/013）。
#[tokio::test]
async fn 系统故障不产生任何可落库文本() {
    // 未启用：连调用都不该发生。
    let fake = FakeTranscriber::replying(200, r#"{"text":"在的"}"#);
    let got =
        inbound_audio_content(&voice_cfg(false), 1, &audio_ref(Some(3), None), None, &fake).await;
    assert!(got.is_none(), "未启用时不该产出文本");
    assert_eq!(fake.calls(), 0, "未启用时不该发起调用");

    // 网关 5xx：我们自己的问题，绝不伪装成"没听清"。
    for (status, body) in [
        (500u16, r#"{"error":"内部错误"}"#),
        (503, r#"{"kind":"engine_unavailable"}"#),
    ] {
        let fake = FakeTranscriber::replying(status, body);
        let got =
            inbound_audio_content(&voice_cfg(true), 1, &audio_ref(Some(3), None), None, &fake)
                .await;
        assert!(got.is_none(), "{status} 不该产出文本");
        assert_eq!(fake.calls(), 1);
    }

    // 连下载 / 连接都没走通（超时、网关没起）。
    let fake = FakeTranscriber::unreachable("调用转写端点失败：连接超时");
    let got =
        inbound_audio_content(&voice_cfg(true), 1, &audio_ref(Some(3), None), None, &fake).await;
    assert!(got.is_none(), "连不上时不该产出文本");
}

/// 成功路径与"带 caption 的失败路径"都要把 caption 保在前面（FR-012）。
#[tokio::test]
async fn 转写与失败路径都保住用户写的旁白() {
    let ok = FakeTranscriber::replying(200, r#"{"text":"今天晚上想吃什么"}"#);
    let got = inbound_audio_content(
        &voice_cfg(true),
        1,
        &audio_ref(Some(3), None),
        Some("昨天会议的录音"),
        &ok,
    )
    .await;
    assert_eq!(
        got.as_deref(),
        Some("昨天会议的录音\n[语音] 今天晚上想吃什么")
    );

    let bad = FakeTranscriber::replying(200, r#"{"text":""}"#);
    let got = inbound_audio_content(
        &voice_cfg(true),
        1,
        &audio_ref(Some(3), None),
        Some("昨天会议的录音"),
        &bad,
    )
    .await;
    assert_eq!(
        got.as_deref(),
        Some("昨天会议的录音\n[对方发来一条语音，但没能听清内容]")
    );

    let too_long = audio_ref(Some(9999), None);
    let got =
        inbound_audio_content(&voice_cfg(true), 1, &too_long, Some("昨天会议的录音"), &bad).await;
    assert_eq!(
        got.as_deref(),
        Some("昨天会议的录音\n[对方发来一条语音，但内容太大没能听清]")
    );
}

/// FR-005/021 的实现不变量：失败路径**只**产出情境标记，绝不在通道层编造面向用户的
/// 回复文案。所有失败输出的集合必须落在这两个常量里——用户看到的那句话只会来自模型。
#[tokio::test]
async fn 失败路径只产出情境标记而非回复文案() {
    let allowed = [
        VOICE_UNINTELLIGIBLE_MARKER,
        VOICE_TOO_LARGE_MARKER,
        EXPIRED_VOICE_PLACEHOLDER,
    ];
    let probes: &[(u16, &str)] = &[
        (200, r#"{"text":""}"#),
        (422, "{}"),
        (400, r#"{"error":"x"}"#),
        (413, r#"{"error":"x"}"#),
        (415, r#"{"error":"x"}"#),
    ];
    for (status, body) in probes {
        let fake = FakeTranscriber::replying(*status, body);
        let got =
            inbound_audio_content(&voice_cfg(true), 1, &audio_ref(Some(3), None), None, &fake)
                .await
                .expect("内容层面失败应有可落库文本");
        assert!(
            allowed.contains(&got.as_str()),
            "失败输出 {got:?} 不在允许的情境标记集合里（通道层不得编造回复文案）"
        );
    }
    // 超限也是同一集合。
    let fake = FakeTranscriber::replying(200, "{}");
    let got = inbound_audio_content(
        &voice_cfg(true),
        1,
        &audio_ref(Some(9999), None),
        None,
        &fake,
    )
    .await
    .expect("超限应有可落库文本");
    assert!(allowed.contains(&got.as_str()));
}

/// 启动自检从 `/health` 里取值：字段齐全时如实反映，缺字段时退化成"不可用"，绝不 panic。
#[test]
fn 自检摘要按health字段取值() {
    let full = serde_json::json!({
        "ok": true,
        "asr": { "engine": "faster-whisper", "importable": true, "model_loaded": false }
    });
    let h = asr_health(&full);
    assert_eq!(h.engine, "faster-whisper");
    assert!(h.importable);
    assert!(!h.model_loaded);

    // 老版本网关（没有 asr 段）、字段类型不对、响应不是对象——都退化成不可用。
    for value in [
        serde_json::json!({ "ok": true }),
        serde_json::json!({ "asr": {} }),
        serde_json::json!({ "asr": { "engine": 7, "importable": "yes" } }),
        serde_json::json!("not an object"),
    ] {
        let h = asr_health(&value);
        assert!(!h.importable, "取值失败时必须报不可用：{value}");
        assert!(!h.model_loaded);
    }
}
