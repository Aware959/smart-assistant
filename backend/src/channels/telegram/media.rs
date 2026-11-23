//! Telegram media 子模块。

use super::*;

use super::super::{status_retryable, with_send_retry, SendErr, SEND_RETRY};

pub(super) async fn media_generate(
    media_url: &str,
    kind: &str,
    prompt: &str,
    timeout: u64,
) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| format!("创建媒体网关客户端失败: {e}"))?;
    let url = format!("{media_url}/generate/{kind}");
    // 只传最必要的字段：图片传 prompt、语音传 text；其余参数（步数、尺寸、音色…）
    // 由网关按自己接的后端决定默认值，Rust 不必关心。
    let body = if kind == "image" {
        serde_json::json!({ "prompt": prompt })
    } else {
        serde_json::json!({ "text": prompt })
    };
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("调用媒体网关失败: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!("媒体网关返回 {status}: {detail}"));
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("读取媒体字节失败: {e}"))
}
pub(super) struct MediaPart {
    method: &'static str,
    field: &'static str,
    filename: String,
}

/// Telegram caption（图片/语音旁白）长度上限（UTF-16 码元，emoji 占 2）。
pub(super) const MAX_CAPTION_LEN: usize = 1024;

/// 旁白是否在 Telegram caption 上限内。超限由 [`send_media`] 整条撤下（不截断）。
pub(super) fn caption_fits(caption: &str) -> bool {
    caption.encode_utf16().count() <= MAX_CAPTION_LEN
}

/// 上传一张本地生成的图片（多部件）。`caption` 为可选随图旁白。
pub(super) async fn send_photo(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    bytes: &[u8],
    caption: Option<&str>,
) -> Result<(), String> {
    send_media(
        client,
        base,
        chat_id,
        MediaPart {
            method: "sendPhoto",
            field: "photo",
            filename: "image.png".to_string(),
        },
        bytes.to_vec(),
        caption.map(str::to_string),
    )
    .await
}

/// 上传一段本地合成的语音（多部件）。Telegram 语音不带 caption（用语音本身表达）。
pub(super) async fn send_voice(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    bytes: &[u8],
    caption: Option<&str>,
) -> Result<(), String> {
    send_media(
        client,
        base,
        chat_id,
        MediaPart {
            method: "sendVoice",
            field: "voice",
            filename: "voice.ogg".to_string(),
        },
        bytes.to_vec(),
        caption.map(str::to_string),
    )
    .await
}

/// 上传一段音频（Telegram 的 `sendAudio`：与语音消息 `sendVoice` 是两个接口）。
pub(super) async fn send_audio(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    bytes: Vec<u8>,
    filename: String,
    caption: Option<&str>,
) -> Result<(), String> {
    send_media(
        client,
        base,
        chat_id,
        MediaPart {
            method: "sendAudio",
            field: "audio",
            filename,
        },
        bytes,
        caption.map(str::to_string),
    )
    .await
}

/// 上传一个通用文件（多部件）。
pub(super) async fn send_document(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    bytes: Vec<u8>,
    filename: String,
    caption: Option<&str>,
) -> Result<(), String> {
    send_media(
        client,
        base,
        chat_id,
        MediaPart {
            method: "sendDocument",
            field: "document",
            filename,
        },
        bytes,
        caption.map(str::to_string),
    )
    .await
}

/// 以 `multipart/form-data` 上传一段媒体字节到 Telegram（`sendPhoto` / `sendVoice` 同构）。
///
/// 与 [`send_action`] 共用同一套重试与错误分类：传输层错误 / 429 / 5xx / 响应截断退避重投，
/// 4xx 与 `ok:false` 属确定性拒绝立即放弃。表单每次重试都重建（字节可克隆，避免 move 后
/// 二次调用丢失），所以闭包里现造表单。`MediaPart` 是 `Copy`，重试闭包每次现造即可。
pub(super) async fn send_media(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    part: MediaPart,
    bytes: Vec<u8>,
    caption: Option<String>,
) -> Result<(), String> {
    // 旁白超限（Telegram caption 上限 1024 个 UTF-16 码元）时整条撤下，而不是截断：
    // 截断会留下半句话，比"没有旁白"更像故障；媒体本身照发，不该为附属品把图弄丢。
    let caption = match caption {
        Some(c) if !caption_fits(&c) => {
            tracing::warn!(
                chat_id,
                units = c.encode_utf16().count(),
                "旁白超过 Telegram 上限，已整条省略（媒体照发）"
            );
            None
        }
        other => other,
    };
    let url = format!("{base}/{}", part.method);
    // 重试闭包是 FnMut（可能跑多次），捕获的值每次都得是新的：文件名跟着字节
    // 一起在闭包内克隆，否则第二次重试就拿不到它了。
    let field = part.field;
    let filename = part.filename.clone();
    with_send_retry(
        &format!("telegram/{}", part.method),
        SEND_RETRY,
        move |_| {
            // 重试闭包是 FnMut（可能跑多次），里面要的值每次都得是新的，故先克隆再 move 进 future。
            let url = url.clone();
            let bytes = bytes.clone();
            let caption = caption.clone();
            let filename = filename.clone();
            let what = part.method;
            async move {
                let mut form = reqwest::multipart::Form::new()
                    .text("chat_id", chat_id.to_string())
                    .part(
                        field,
                        reqwest::multipart::Part::bytes(bytes).file_name(filename),
                    );
                if let Some(c) = caption {
                    form = form.text("caption", c);
                }
                let resp = client
                    .post(&url)
                    .timeout(Duration::from_secs(60))
                    .multipart(form)
                    .send()
                    .await
                    .map_err(|e| SendErr::Retryable {
                        reason: format!("上传 {what} 失败: {e}"),
                        retry_after: None,
                    })?;

                let status = resp.status();
                let body = match resp.json::<SendResponse>().await {
                    Ok(b) => b,
                    Err(e) if status.is_success() => {
                        return Err(SendErr::Retryable {
                            reason: format!("解析响应失败: {e}"),
                            retry_after: None,
                        });
                    }
                    Err(_) => SendResponse::default(),
                };

                if !status.is_success() || !body.ok {
                    let reason = body
                        .description
                        .filter(|d| !d.trim().is_empty())
                        .unwrap_or_else(|| status.to_string());
                    if status_retryable(status.as_u16()) {
                        return Err(SendErr::Retryable {
                            reason: format!("服务端 {status}: {reason}"),
                            retry_after: body
                                .parameters
                                .and_then(|p| p.retry_after)
                                .map(Duration::from_secs),
                        });
                    }
                    return Err(SendErr::Fatal(format!("发送被拒绝({status}): {reason}")));
                }
                Ok(())
            }
        },
    )
    .await
}

pub(super) async fn send_typing(client: &reqwest::Client, base: &str, chat_id: i64) {
    let _ = client
        .post(format!("{base}/sendChatAction"))
        .timeout(Duration::from_secs(10))
        .json(&serde_json::json!({ "chat_id": chat_id, "action": "typing" }))
        .send()
        .await;
}

/// 设置「正在做某事」的状态动作：生成图片用 `upload_photo`、合成语音用 `record_voice`，
/// 让用户知道我们在处理而不是卡住了。失败仅记日志，不影响后续上传。
pub(super) async fn send_chat_action(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    action: &str,
) {
    let _ = client
        .post(format!("{base}/sendChatAction"))
        .timeout(Duration::from_secs(10))
        .json(&serde_json::json!({ "chat_id": chat_id, "action": action }))
        .send()
        .await;
}
