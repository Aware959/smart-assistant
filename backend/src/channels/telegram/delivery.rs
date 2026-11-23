//! Telegram delivery 子模块。

use super::*;

pub(super) struct Delivery {
    reply: Reply,
    pub(super) ack: std::sync::mpsc::SyncSender<std::result::Result<Option<u8>, String>>,
}

/// 通道侧的投递口：对话循环（阻塞线程）通过它把片段交给 [`deliver_worker`] 真正发送。
///
/// 为什么要工人而不是在循环里直接发：`chat_stream` 跑在阻塞线程上，直接对 token 发就得
/// 在阻塞线程里 `block_on` 异步请求；交给同一 runtime 上的工人，既不必嵌套 runtime，
/// 也能把打字态与真人节奏留在异步侧。循环发一个、阻塞等一个回执，顺序天然不乱。
#[derive(Clone)]
pub(super) struct TgDeliver {
    tx: tokio::sync::mpsc::UnboundedSender<Delivery>,
    /// 本投递口至今成功送达的片段数。失败轮次据此判断"用户是否已经看到过东西"——
    /// 看到过就不再整轮重试（见 [`should_retry_after_failure`]）。
    delivered: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl TgDeliver {
    pub(super) fn new(tx: tokio::sync::mpsc::UnboundedSender<Delivery>) -> Self {
        Self {
            tx,
            delivered: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// 本投递口至今成功送达的片段数（只读，各克隆共享同一个计数）。
    pub(super) fn delivered_count(&self) -> usize {
        self.delivered.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 投递一个片段并等待落定。骰子返回服务端点数；文本/贴纸返回 `None`。
    pub(super) fn deliver(&self, reply: &Reply) -> std::result::Result<Option<u8>, String> {
        let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(0);
        self.tx
            .send(Delivery {
                reply: reply.clone(),
                ack: ack_tx,
            })
            .map_err(|_| "投递通道已关闭".to_string())?;
        let result = ack_rx.recv().map_err(|_| "投递工人未回执".to_string())?;
        // 只有真正送达（工人发送成功）才计数：失败的重试决策看的是"用户看到了几条"。
        if result.is_ok() {
            self.delivered
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        result
    }
}

/// 一轮对话失败后，要不要入队重试。
///
/// 有一条已经送达就不再重试：重试是**整轮重跑**（模型看不到首轮已经发出去的话），
/// 会把同样的话再说一遍，用户收到重复消息——比"后半截没说出来"更糟。一条都没送达时
/// 重试是安全的（用户什么都没看到），也正是重试机制要保的场景。
pub(super) fn should_retry_after_failure(delivered: usize) -> bool {
    delivered == 0
}

/// 投递工人：按到达顺序逐条发送，维持打字态与真人一句接一句的节奏。
///
/// 队列空（所有 sender 被丢弃）即结束；发送结果（骰子点数）经回执口交还对话循环。
pub(super) async fn deliver_worker(
    client: reqwest::Client,
    base: String,
    chat_id: i64,
    media_url: String,
    media_timeout: u64,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Delivery>,
) {
    let mut index = 0usize;
    while let Some(Delivery { reply, ack }) = rx.recv().await {
        if index > 0 {
            tokio::time::sleep(super::super::pacing_delay(index)).await;
        }
        index += 1;
        send_typing(&client, &base, chat_id).await;
        let result = match &reply {
            Reply::Text(text) => send_text(&client, &base, chat_id, text).await.map(|_| None),
            Reply::Dice { emoji } => send_dice(&client, &base, chat_id, emoji).await.map(Some),
            Reply::Sticker { file_id } => send_sticker(&client, &base, chat_id, file_id)
                .await
                .map(|_| None),
            // 本机生成的图片：先告知"正在上传图片"，再调网关出图、上传到 Telegram。
            // 生成失败直接上抛，交由对话循环的失败分支发兜底话术。
            Reply::Photo { prompt, caption } => {
                send_chat_action(&client, &base, chat_id, "upload_photo").await;
                match media_generate(&media_url, "image", prompt, media_timeout).await {
                    Ok(bytes) => send_photo(&client, &base, chat_id, &bytes, caption.as_deref())
                        .await
                        .map(|_| None),
                    Err(e) => {
                        tracing::error!(error = %e, chat_id, "图片生成失败");
                        Err(e)
                    }
                }
            }
            // 本机合成的语音：先告知"正在录音"，再调网关合成、上传到 Telegram。
            Reply::Voice { text } => {
                send_chat_action(&client, &base, chat_id, "record_voice").await;
                match media_generate(&media_url, "voice", text, media_timeout).await {
                    Ok(bytes) => send_voice(&client, &base, chat_id, &bytes, None)
                        .await
                        .map(|_| None),
                    Err(e) => {
                        tracing::error!(error = %e, chat_id, "语音合成失败");
                        Err(e)
                    }
                }
            }
            // MCP 工具带回的**成品**媒体：字节已经在手，按 MIME 选投递方式即可，
            // 不必再走"生成意图 → 网关 → 字节"那条路。
            Reply::Media {
                bytes,
                mime,
                filename,
                caption,
            } => {
                let kind = crate::core::agent::MediaKind::from_mime(mime);
                let filename = filename.clone().unwrap_or_else(|| default_filename(kind));
                match kind {
                    crate::core::agent::MediaKind::Image => {
                        send_chat_action(&client, &base, chat_id, "upload_photo").await;
                        send_photo(
                            &client,
                            &base,
                            chat_id,
                            bytes.as_slice(),
                            caption.as_deref(),
                        )
                        .await
                        .map(|_| None)
                    }
                    crate::core::agent::MediaKind::Audio => {
                        send_chat_action(&client, &base, chat_id, "record_voice").await;
                        // Telegram 的语音只吃 ogg/opus；别的音频按普通音频发，
                        // 否则服务端会拒。
                        if is_voice_ogg(mime) {
                            send_voice(
                                &client,
                                &base,
                                chat_id,
                                bytes.as_slice(),
                                caption.as_deref(),
                            )
                            .await
                            .map(|_| None)
                        } else {
                            send_audio(
                                &client,
                                &base,
                                chat_id,
                                bytes.clone(),
                                filename,
                                caption.as_deref(),
                            )
                            .await
                            .map(|_| None)
                        }
                    }
                    crate::core::agent::MediaKind::File => send_document(
                        &client,
                        &base,
                        chat_id,
                        bytes.clone(),
                        filename,
                        caption.as_deref(),
                    )
                    .await
                    .map(|_| None),
                }
            }
        };
        // 回执失败 = 对话循环已放弃（超时 / 异常），工人无需再做别的。
        let _ = ack.send(result);
    }
}

/// 成品媒体没有文件名时给的兜底名（Telegram 只把它当展示名，不校验扩展名）。
fn default_filename(kind: crate::core::agent::MediaKind) -> String {
    match kind {
        crate::core::agent::MediaKind::Image => "image.png".to_string(),
        crate::core::agent::MediaKind::Audio => "audio.ogg".to_string(),
        crate::core::agent::MediaKind::File => "file".to_string(),
    }
}

/// 该音频能否走 `sendVoice`（Telegram 只接受 ogg/opus 两种语音容器）。
fn is_voice_ogg(mime: &str) -> bool {
    let mime = mime.trim().to_ascii_lowercase();
    mime.contains("ogg") || mime.contains("opus")
}
