//! Telegram Bot 接入（长轮询）。
//!
//! - 启动时用 `getMe` 校验 token，并丢弃历史积压 update；
//! - 每个 chat 映射独立 session（经 [`crate::core::session::Sessions`] 的会话映射解析），多轮上下文互不串扰；
//! - 文本超长时按 4096 字符分片发送；
//! - 代理优先级：`TELEGRAM_PROXY` → `HTTPS_PROXY` → `ALL_PROXY`
//!   （api.telegram.org 在部分地区不可直连）。

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::core::agent::Agent;
use crate::core::session::{classify_inbound, InboundFreshness, Sessions, TurnTicket};
use crate::{ChatInput, ChatOutput, Reply};

const CHANNEL: &str = "telegram";
/// Telegram 单条消息文本上限（UTF-16 码元，见 [`super::chunk_text_utf16`]）。
const MAX_MESSAGE_LEN: usize = 4096;
/// getUpdates 长轮询超时（秒）。
const POLL_TIMEOUT_SECS: u64 = 50;
/// 网络异常后的退避等待（秒）。
const RETRY_DELAY_SECS: u64 = 5;

/// 重试扫表周期（秒）：每隔多久醒来一次扫 `pending_retry` 表。
const SWEEP_INTERVAL_SECS: u64 = 15;
/// 失败轮次的最大重试次数（含首次失败后的重试）。
const MAX_RETRY_ATTEMPTS: i64 = 5;
/// 持久重试退避步长（秒），按"已失败次数"索引（从 1 起），用尽后保持最后一步长。
const RETRY_BACKOFF_SECS: &[u64] = &[30, 120, 600, 1800];
/// 超时轮次下次重试的最小间隔（秒）：须大于单次 LLM 调用上限（genai 侧 `CHAT_TIMEOUT`=300s），
/// 否则被 `AI_DEADLINE_SECS` 超时打断的旧阻塞任务可能仍在跑，会与新重试并发写同一会话。
const RETRY_TIMEOUT_GRACE_SECS: u64 = 360;

/// 语音转写结果落库时加的前缀：让"这条源自语音"在时间线上可辨（FR-009）。
///
/// **只进 `messages.content`**（供模型与记忆使用），**绝不回帖给用户**——用户不该看到
/// 转写文字（FR-010）。
const VOICE_PREFIX: &str = "[语音] ";

/// 过期语音的存档占位。只入库、不会被回复，所以它不需要是"给模型看的情境描述"。
const EXPIRED_VOICE_PLACEHOLDER: &str = "[对方发来一条语音（已过期）]";

/// 没听出内容时的落库标记。
///
/// ⚠️ 它是**给模型看的情境描述**，不是"回给用户的话术"——用户看到的那句由模型现场生成
/// （FR-005）。仓库里 MUST NOT 出现任何固定回复文案。
const VOICE_UNINTELLIGIBLE_MARKER: &str = "[对方发来一条语音，但没能听清内容]";

/// 音频超出处理上限（时长 / 体积）时的落库标记。同样是给模型看的情境描述。
///
/// 与"没听清"分开，是因为这是**我们能确定的原因**：不是识别不了，是它太大了——模型据此
/// 可以选择合适的回应方式（FR-003/005）。
const VOICE_TOO_LARGE_MARKER: &str = "[对方发来一条语音，但内容太大没能听清]";

/// Telegram Bot API `getFile` 的下载硬上限（20 MB）：超过它连下载都做不到，
/// 因此必须在**发起下载之前**拦下（见 [`voice_plan`]）。
const MAX_VOICE_BYTES: i64 = 20 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    #[serde(default)]
    message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    #[serde(default)]
    text: Option<String>,
    /// 贴纸消息：没有 `text`，内容在这一段里。目前只取 `emoji` 作可读标签。
    #[serde(default)]
    sticker: Option<TgSticker>,
    /// 语音条：用户按住说话录的，Telegram 侧就是 OGG/OPUS。
    #[serde(default)]
    voice: Option<TgVoice>,
    /// 音频文件：用户以"文件"方式发的录音/音乐（可能是 mp3、m4a…）。
    #[serde(default)]
    audio: Option<TgAudio>,
    /// 音频文件可带的文字说明。语音条本身不带 caption，故它实际只与 `audio` 同时出现。
    #[serde(default)]
    caption: Option<String>,
    chat: TgChat,
    /// 消息发出时刻（Unix 秒，服务端收到即记），等于用户按下发送的世界时刻。
    #[serde(default)]
    date: Option<i64>,
}

/// 入站语音条（只取转写用得上的字段）。
#[derive(Debug, Deserialize)]
struct TgVoice {
    file_id: String,
    /// 时长（秒）。用于**前置拦截**：超过 `VOICE_TRANSCRIBE_MAX_SECS` 就不下载（FR-003）。
    #[serde(default)]
    duration: Option<i64>,
    /// Telegram 给的 MIME（语音条通常 `audio/ogg`），透传给网关当 `Content-Type`。
    #[serde(default)]
    mime_type: Option<String>,
    /// 字节数。超过 Bot API 的 20 MB 下载上限就不必白跑一趟（FR-003）。
    #[serde(default)]
    file_size: Option<i64>,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// 入站音频文件。字段与 [`TgVoice`] 重合，另多 `file_name` / `title`。
#[derive(Debug, Deserialize)]
struct TgAudio {
    file_id: String,
    #[serde(default)]
    duration: Option<i64>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    file_size: Option<i64>,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// 音频附件的**公共视角**：转写需要的字段 + 排错用的文件信息，不必分它是语音条还是音频文件。
#[derive(Debug)]
struct AudioRef<'a> {
    file_id: &'a str,
    duration: Option<i64>,
    mime_type: Option<&'a str>,
    file_size: Option<i64>,
    file_name: Option<&'a str>,
    title: Option<&'a str>,
    /// 语音条为 true。仅用于在 `mime_type` 缺失时决定回退成 `audio/ogg` 还是 `audio/mpeg`。
    is_voice: bool,
}

impl AudioRef<'_> {
    /// 送给网关的 `Content-Type`：优先用 Telegram 给的 `mime_type`，缺省按类型回退。
    fn mime(&self) -> &str {
        self.mime_type.unwrap_or(if self.is_voice {
            "audio/ogg"
        } else {
            "audio/mpeg"
        })
    }
}

impl TgMessage {
    /// 取本条消息里的音频附件（语音条优先于音频文件）。没有则 `None`。
    fn audio_attachment(&self) -> Option<AudioRef<'_>> {
        if let Some(v) = self.voice.as_ref() {
            return Some(AudioRef {
                file_id: &v.file_id,
                duration: v.duration,
                mime_type: v.mime_type.as_deref(),
                file_size: v.file_size,
                file_name: v.file_name.as_deref(),
                title: v.title.as_deref(),
                is_voice: true,
            });
        }
        self.audio.as_ref().map(|a| AudioRef {
            file_id: &a.file_id,
            duration: a.duration,
            mime_type: a.mime_type.as_deref(),
            file_size: a.file_size,
            file_name: a.file_name.as_deref(),
            title: a.title.as_deref(),
            is_voice: false,
        })
    }
}

/// 入站贴纸（只取生成可读文本用得上的字段）。
#[derive(Debug, Deserialize)]
struct TgSticker {
    #[serde(default)]
    emoji: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

/// `getStickerSet` 返回的贴纸包（只要生成目录用得上的几个字段）。
#[derive(Debug, Deserialize)]
struct StickerSet {
    #[serde(default)]
    name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    stickers: Vec<StickerItem>,
}

/// 贴纸包里的一张贴纸。`emoji` 是 Telegram 给的标签，这里被当作模型的选择键。
#[derive(Debug, Deserialize)]
struct StickerItem {
    #[serde(default)]
    file_id: String,
    #[serde(default)]
    emoji: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendMessage<'a> {
    chat_id: i64,
    text: &'a str,
}

/// `sendMessage` 响应体。Bot API 会用 200 携带业务错误（`ok:false`），
/// 限流时在 `parameters.retry_after` 里给出应当等待的秒数。
#[derive(Debug, Clone, Default, Deserialize)]
struct SendResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<SendParameters>,
    /// `sendDice` 等业务的 `result` 载荷（骰子点数在 `result.dice.value`）。
    /// 投骰前不知道点数，必须透传出来交给内核，否则模型只能瞎编。
    #[serde(default)]
    result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct SendParameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

fn api_base(token: &str) -> String {
    format!("https://api.telegram.org/bot{token}")
}

fn build_client() -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder();
    let proxy = Config::get()
        .telegram_proxy
        .clone()
        .or_else(|| std::env::var("HTTPS_PROXY").ok())
        .or_else(|| std::env::var("ALL_PROXY").ok());
    if let Some(proxy) = proxy {
        builder =
            builder.proxy(reqwest::Proxy::all(&proxy).map_err(|e| format!("无效的代理地址: {e}"))?);
    }
    builder
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

/// 复用同一套代理配置的 Telegram client 构造（长轮询 `run` 与重试扫表 `run_retry_sweep` 共用）。
pub(crate) fn telegram_client() -> Result<reqwest::Client, String> {
    build_client()
}

use super::chunk_text_utf16;
use super::{status_retryable, with_send_retry, SendErr, SEND_RETRY};

async fn api_get<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, String> {
    let resp = client
        .get(url)
        .timeout(Duration::from_secs(POLL_TIMEOUT_SECS + 20))
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    match resp.status().as_u16() {
        401 => return Err("token 无效（401），请检查 TELEGRAM_BOT_TOKEN".to_string()),
        409 => return Err("409 冲突：可能有另一个实例在轮询同一 bot".to_string()),
        _ => {}
    }
    resp.json::<T>()
        .await
        .map_err(|e| format!("解析响应失败: {e}"))
}

/// 发一条文本，超长自动分片。
///
/// 分片按 **UTF-16 码元**计数（不是字符数）：Telegram 单条上限 4096 个 UTF-16 码元，
/// emoji 等非 BMP 字符占 2——按字符数分片会切出超限片段被服务端 400 拒收。
///
/// 每片独立重试：已送达的前片不会因后片最终失败而被重发（否则用户会收到重复内容）。
/// 只对"重投可能成功"的失败退避重试——传输层错误、429 限流、5xx、响应被截断；
/// 4xx 与 `ok:false` 属确定性拒绝，立即放弃，把失败交回调用方。
async fn send_text(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    text: &str,
) -> Result<(), String> {
    for chunk in chunk_text_utf16(text, MAX_MESSAGE_LEN) {
        let chunk = &chunk;
        with_send_retry("telegram/sendMessage", SEND_RETRY, |_| async move {
            let resp = client
                .post(format!("{base}/sendMessage"))
                .timeout(Duration::from_secs(30))
                .json(&SendMessage {
                    chat_id,
                    text: chunk,
                })
                .send()
                .await
                .map_err(|e| SendErr::Retryable {
                    reason: format!("发送消息失败: {e}"),
                    retry_after: None,
                })?;

            let status = resp.status();
            let body = match resp.json::<SendResponse>().await {
                Ok(b) => b,
                // 2xx 却读不出 JSON：响应多半在传输途中被掐断，重投有望送达。
                Err(e) if status.is_success() => {
                    return Err(SendErr::Retryable {
                        reason: format!("解析响应失败: {e}"),
                        retry_after: None,
                    });
                }
                // 错误状态读不到 body 也不妨碍按状态码判定。
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
                        // 限流按服务端给的秒数等，别把余下几次重投全耗在 429 上。
                        retry_after: body
                            .parameters
                            .and_then(|p| p.retry_after)
                            .map(Duration::from_secs),
                    });
                }
                return Err(SendErr::Fatal(format!("发送被拒绝({status}): {reason}")));
            }
            Ok(())
        })
        .await?;
    }
    Ok(())
}

/// 发一次「一个动作 + 一枚 JSON 载荷」的请求（`sendDice` / `sendSticker` …）。
///
/// 这类请求除了端点与载荷，重试与错误分类完全同构，故收在一处：传输层错误、
/// 429 限流、5xx、响应被截断退避重投，4xx 与 `ok:false` 属确定性拒绝立即放弃。
async fn send_action(
    client: &reqwest::Client,
    what: &str,
    url: &str,
    payload: &serde_json::Value,
) -> Result<SendResponse, String> {
    // 把成功响应的 `result` 透传出来（骰子点数就藏在里面）：用内部可变捕获在重试闭包里
    // 写回，最终返回给调用方。用 `Arc<Mutex>`（而非 `Rc<RefCell>`）是因为投递工人跑在
    // async 任务里，整个 future 必须 `Send`——`Rc` 会把它钉在单线程上。
    let captured = std::sync::Arc::new(std::sync::Mutex::new(SendResponse::default()));
    let cap = captured.clone();
    with_send_retry(&format!("telegram/{what}"), SEND_RETRY, move |_| {
        let cap = cap.clone();
        async move {
            let resp = client
                .post(url)
                .timeout(Duration::from_secs(30))
                .json(payload)
                .send()
                .await
                .map_err(|e| SendErr::Retryable {
                    reason: format!("发送 {what} 失败: {e}"),
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
                        // 限流按服务端给的秒数等，别把余下几次重投全耗在 429 上。
                        retry_after: body
                            .parameters
                            .and_then(|p| p.retry_after)
                            .map(Duration::from_secs),
                    });
                }
                return Err(SendErr::Fatal(format!("发送被拒绝({status}): {reason}")));
            }
            *cap.lock().expect("发送响应互斥锁中毒") = body;
            Ok(())
        }
    })
    .await?;
    let body = captured.lock().expect("发送响应互斥锁中毒").clone();
    Ok(body)
}

/// 发一枚动画骰子（Telegram 服务端渲染，无素材上传）。
async fn send_dice(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    emoji: &str,
) -> Result<u8, String> {
    let resp = send_action(
        client,
        "sendDice",
        &format!("{base}/sendDice"),
        &serde_json::json!({ "chat_id": chat_id, "emoji": emoji }),
    )
    .await?;
    dice_value(&resp.result)
}

/// 从 `sendDice` 响应里取出骰子点数（1–6）。
///
/// Telegram 的骰子消息在服务端就定好了点数，响应 `result.dice.value` 里立刻带着，
/// 所以发完骰子就能知道掷出了几——这点必须交回内核，否则模型只能瞎编。
fn dice_value(result: &Option<serde_json::Value>) -> Result<u8, String> {
    let v = result
        .as_ref()
        .and_then(|r| r.get("dice"))
        .and_then(|d| d.get("value"))
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "骰子响应缺少 result.dice.value".to_string())?;
    if !(1..=6).contains(&v) {
        return Err(format!("骰子点数越界: {v}"));
    }
    Ok(v as u8)
}

/// 发一张贴纸。`file_id` 来自 [`crate::core::sticker::StickerCatalog`]，
/// 只在当前 bot 有效——目录过期（换 bot / 包被删）由服务端拒绝，这里如实上抛。
async fn send_sticker(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    file_id: &str,
) -> Result<(), String> {
    send_action(
        client,
        "sendSticker",
        &format!("{base}/sendSticker"),
        &serde_json::json!({ "chat_id": chat_id, "sticker": file_id }),
    )
    .await
    .map(|_| ())
}

/// 调本机媒体网关生成媒体，返回原始字节。
///
/// 网关是独立的本地进程（见仓库 `media-gateway/`），Rust 只跟它做 HTTP，对它背后的
/// 扩散软件 / TTS 无感知。`kind` 为 `image` / `voice`，对应网关的
/// `/generate/image` / `/generate/voice`。
///
/// ⚠️ 必须显式 `.no_proxy()`：本机 Clash 系统代理会把 `127.0.0.1` 也路由进代理，
/// 否则这里会连不上网关。切勿改用全局 `NO_PROXY=*`——那会连 Telegram 轮询依赖的代理一并绕过。
async fn media_generate(
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

/// `getFile` 响应里我们只要 `file_path`（真正的下载地址由它拼出来）。
#[derive(Debug, Deserialize)]
struct TgFile {
    #[serde(default)]
    file_path: Option<String>,
}

/// 取一条语音/音频的原始字节 → 送给本机网关转写，返回 `(状态码, 响应体)`。
///
/// 状态码语义由 [`voice_outcome`] 解释。`Err` 只表示**下载或连接这一步就没走通**
/// （网络、超时、Telegram 拒绝、网关不在）——即系统故障路径，调用方据此直接放弃本轮。
///
/// 两步都是必需的：Telegram 的文件下载与 API 调用不在同一个前缀下
/// （`/bot<token>/…` 对 `/file/bot<token>/…`）。
///
/// ⚠️ 网关 client 必须显式 `.no_proxy()`：本机 Clash 系统代理会把 `127.0.0.1` 也路由进
/// 代理，否则这里会连不上网关（理由同 [`media_generate`]）。
async fn transcribe_voice(
    tg_client: &reqwest::Client,
    token: &str,
    gateway_url: &str,
    timeout: u64,
    file_id: &str,
    mime_type: &str,
) -> Result<(u16, String), String> {
    // 1) getFile：拿到服务端的文件路径。file_id 是 Bot API 给的 URL-safe 串，直接拼即可。
    let url = format!("{}/getFile?file_id={file_id}", api_base(token));
    let file: ApiResponse<TgFile> = api_get(tg_client, &url).await?;
    if !file.ok {
        return Err(format!(
            "getFile 失败：{}",
            file.description.unwrap_or_default()
        ));
    }
    let path = file
        .result
        .and_then(|f| f.file_path)
        .ok_or_else(|| "getFile 未返回 file_path".to_string())?;

    // 2) 下载原始字节（注意是 `/file/bot<token>/` 前缀，不是 `/bot<token>/`）。
    let audio = tg_client
        .get(format!("https://api.telegram.org/file/bot{token}/{path}"))
        .timeout(Duration::from_secs(timeout))
        .send()
        .await
        .map_err(|e| format!("下载语音失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("下载语音失败：{e}"))?
        .bytes()
        .await
        .map_err(|e| format!("读取语音字节失败：{e}"))?
        .to_vec();

    // 3) 送网关：请求体是**原始音频字节**，不是 JSON、不是 multipart、不是 base64。
    //    网关内部用 PyAV 直吃 OGG/OPUS，不必在这里做任何转码。
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| format!("创建转写客户端失败: {e}"))?;
    let resp = client
        .post(format!("{gateway_url}/transcribe"))
        .header(reqwest::header::CONTENT_TYPE, mime_type)
        .body(audio)
        .send()
        .await
        .map_err(|e| format!("调用转写端点失败：{e}"))?;
    let status = resp.status().as_u16();
    // 非 2xx 也要把响应体读出来——错误详情在里面，是排错与分流的依据。
    let body = resp.text().await.unwrap_or_default();
    Ok((status, body))
}

/// 一次媒体上传的 Telegram 端点三要素：方法名 / 多部件字段名 / 上传文件名。
///
/// 三者总是成组出现（`sendPhoto`↔`photo`↔`image.png`），打包成一个值既省去逐个传参，
/// 也避免调用方把字段名和方法名配错。
#[derive(Clone, Copy)]
struct MediaPart {
    method: &'static str,
    field: &'static str,
    filename: &'static str,
}

/// Telegram caption（图片/语音旁白）长度上限（UTF-16 码元，emoji 占 2）。
const MAX_CAPTION_LEN: usize = 1024;

/// 旁白是否在 Telegram caption 上限内。超限由 [`send_media`] 整条撤下（不截断）。
fn caption_fits(caption: &str) -> bool {
    caption.encode_utf16().count() <= MAX_CAPTION_LEN
}

/// 上传一张本地生成的图片（多部件）。`caption` 为可选随图旁白。
async fn send_photo(
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
            filename: "image.png",
        },
        bytes.to_vec(),
        caption.map(str::to_string),
    )
    .await
}

/// 上传一段本地合成的语音（多部件）。Telegram 语音不带 caption（用语音本身表达）。
async fn send_voice(
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
            filename: "voice.ogg",
        },
        bytes.to_vec(),
        caption.map(str::to_string),
    )
    .await
}

/// 以 `multipart/form-data` 上传一段媒体字节到 Telegram（`sendPhoto` / `sendVoice` 同构）。
///
/// 与 [`send_action`] 共用同一套重试与错误分类：传输层错误 / 429 / 5xx / 响应截断退避重投，
/// 4xx 与 `ok:false` 属确定性拒绝立即放弃。表单每次重试都重建（字节可克隆，避免 move 后
/// 二次调用丢失），所以闭包里现造表单。`MediaPart` 是 `Copy`，重试闭包每次现造即可。
async fn send_media(
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
    with_send_retry(
        &format!("telegram/{}", part.method),
        SEND_RETRY,
        move |_| {
            // 重试闭包是 FnMut（可能跑多次），里面要的值每次都得是新的，故先克隆再 move 进 future。
            let url = url.clone();
            let bytes = bytes.clone();
            let caption = caption.clone();
            let what = part.method;
            async move {
                let mut form = reqwest::multipart::Form::new()
                    .text("chat_id", chat_id.to_string())
                    .part(
                        part.field,
                        reqwest::multipart::Part::bytes(bytes).file_name(part.filename),
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

async fn send_typing(client: &reqwest::Client, base: &str, chat_id: i64) {
    let _ = client
        .post(format!("{base}/sendChatAction"))
        .timeout(Duration::from_secs(10))
        .json(&serde_json::json!({ "chat_id": chat_id, "action": "typing" }))
        .send()
        .await;
}

/// 设置「正在做某事」的状态动作：生成图片用 `upload_photo`、合成语音用 `record_voice`，
/// 让用户知道我们在处理而不是卡住了。失败仅记日志，不影响后续上传。
async fn send_chat_action(client: &reqwest::Client, base: &str, chat_id: i64, action: &str) {
    let _ = client
        .post(format!("{base}/sendChatAction"))
        .timeout(Duration::from_secs(10))
        .json(&serde_json::json!({ "chat_id": chat_id, "action": action }))
        .send()
        .await;
}

/// 一轮投递请求：一个待发片段 + 把结果带回**同步**对话循环的回执口。
struct Delivery {
    reply: Reply,
    ack: std::sync::mpsc::SyncSender<std::result::Result<Option<u8>, String>>,
}

/// 通道侧的投递口：对话循环（阻塞线程）通过它把片段交给 [`deliver_worker`] 真正发送。
///
/// 为什么要工人而不是在循环里直接发：`chat_stream` 跑在阻塞线程上，直接对 token 发就得
/// 在阻塞线程里 `block_on` 异步请求；交给同一 runtime 上的工人，既不必嵌套 runtime，
/// 也能把打字态与真人节奏留在异步侧。循环发一个、阻塞等一个回执，顺序天然不乱。
#[derive(Clone)]
struct TgDeliver {
    tx: tokio::sync::mpsc::UnboundedSender<Delivery>,
    /// 本投递口至今成功送达的片段数。失败轮次据此判断"用户是否已经看到过东西"——
    /// 看到过就不再整轮重试（见 [`should_retry_after_failure`]）。
    delivered: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl TgDeliver {
    fn new(tx: tokio::sync::mpsc::UnboundedSender<Delivery>) -> Self {
        Self {
            tx,
            delivered: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// 本投递口至今成功送达的片段数（只读，各克隆共享同一个计数）。
    fn delivered_count(&self) -> usize {
        self.delivered.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 投递一个片段并等待落定。骰子返回服务端点数；文本/贴纸返回 `None`。
    fn deliver(&self, reply: &Reply) -> std::result::Result<Option<u8>, String> {
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
fn should_retry_after_failure(delivered: usize) -> bool {
    delivered == 0
}

/// 投递工人：按到达顺序逐条发送，维持打字态与真人一句接一句的节奏。
///
/// 队列空（所有 sender 被丢弃）即结束；发送结果（骰子点数）经回执口交还对话循环。
async fn deliver_worker(
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
            tokio::time::sleep(super::pacing_delay(index)).await;
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
        };
        // 回执失败 = 对话循环已放弃（超时 / 异常），工人无需再做别的。
        let _ = ack.send(result);
    }
}

/// 本轮对话失败（或超时）时把"待重试"落库。失败**不再向用户发声**——原兜底话术
/// "刚才走神了，再发一次试试。"会误导用户重发，而用户消息其实已在对话流水线里落库；
/// 这里改为入队，由 [`run_retry_sweep`] 后续静默重试，成功才把回复送达。
///
/// 重试目标**优先取更新的那条用户消息**（本轮跑着的时候用户又说了话）：重跑会用
/// `exclude_id` 把历史截到触发消息为止（见 `context::fetch_history`），若仍以本轮触发
/// 消息为水位线，插话内容会被截掉、模型看不见——而用户最后说的那句才是最该回应的。
///
/// `timeout` 为真表示本轮是被 `AI_DEADLINE_SECS` 超时打断——其内部的阻塞任务可能仍在跑，
/// 故下次重试时刻推到超时之后，避免与仍在运行的旧任务并发写同一会话。
fn schedule_retry(
    session: &Sessions,
    session_id: &str,
    chat_id: i64,
    content: &str,
    user_time: Option<String>,
    user_message_id: &str,
    timeout: bool,
) {
    // 会话里出现了更新的用户消息：重跑它，本轮触发消息退化成普通历史。
    let (content, user_time, user_message_id) =
        match session.newer_user_message(session_id, user_message_id) {
            Ok(Some(m)) => (m.content, Some(m.created_at), m.id),
            Ok(None) => (content.to_string(), user_time, user_message_id.to_string()),
            Err(e) => {
                tracing::warn!(error = %e, chat_id, "读取更新消息失败，按本轮触发消息重试");
                (content.to_string(), user_time, user_message_id.to_string())
            }
        };
    // 超时轮次要把下次重试推到旧阻塞任务收尾之后；普通失败按首档退避。
    let delay_secs = if timeout {
        RETRY_TIMEOUT_GRACE_SECS as i64
    } else {
        RETRY_BACKOFF_SECS[0] as i64
    };
    if let Err(e) = session.enqueue_retry_for(
        session_id,
        &chat_id.to_string(),
        &user_message_id,
        &content,
        user_time,
        delay_secs,
    ) {
        tracing::error!(error = %e, chat_id, "待重试入队失败");
    }
}

/// 计算第 `attempts` 次失败后的下次重试时刻（attempts 从 1 起，等于已重试次数 + 1）。
///
/// 退避步长按 [`RETRY_BACKOFF_SECS`] 索引，超过后保持最后一步长；超时场景至少退到
/// [`RETRY_TIMEOUT_GRACE_SECS`] 之后，确保旧阻塞任务已收尾、不与新重试并发。
fn next_backoff(attempts: i64, now: DateTime<Utc>, timeout: bool) -> DateTime<Utc> {
    let base = RETRY_BACKOFF_SECS
        .get((attempts - 1) as usize)
        .copied()
        .unwrap_or(*RETRY_BACKOFF_SECS.last().unwrap_or(&30));
    let secs = if timeout {
        base.max(RETRY_TIMEOUT_GRACE_SECS)
    } else {
        base
    };
    now + chrono::Duration::seconds(secs as i64)
}

/// 主动推送：与长轮询解耦，独立构建 client 直接发送（Telegram 无推送窗口限制）。
pub(crate) async fn push_send(chat_id: i64, text: &str) -> Result<(), String> {
    let token = Config::get().telegram_bot_token.trim().to_string();
    if token.is_empty() {
        return Err("未配置 TELEGRAM_BOT_TOKEN".to_string());
    }
    let client = build_client()?;
    send_text(&client, &api_base(&token), chat_id, text).await
}

/// 贴纸包名做 query 编码。Telegram 的名字实际只含 `[A-Za-z0-9_-]`，
/// 这里仍然逐字节编码，免得一个意外字符把请求打成 400 且看不出原因。
fn encode_set_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 拉取一个公开贴纸包，生成**当前 bot 可用**的贴纸目录。
///
/// 这是 `sa-sticker fetch` 的网络侧。`file_id` 是 bot 专属的——同一张贴纸换个
/// bot 就发不出去——所以目录只能由当前 bot 现场拉，不能从别处拷一份静态数据来用。
pub async fn fetch_sticker_set(name: &str) -> Result<crate::core::sticker::StickerCatalog, String> {
    let token = Config::get().telegram_bot_token.trim().to_string();
    if token.is_empty() {
        return Err("未配置 TELEGRAM_BOT_TOKEN，无法拉取贴纸包".to_string());
    }
    let client = build_client()?;
    let base = api_base(&token);
    let resp: ApiResponse<StickerSet> = api_get(
        &client,
        &format!("{base}/getStickerSet?name={}", encode_set_name(name)),
    )
    .await?;
    if !resp.ok {
        return Err(format!(
            "getStickerSet 失败：{}",
            resp.description.unwrap_or_default()
        ));
    }
    let set = resp
        .result
        .ok_or_else(|| "getStickerSet 未返回贴纸包".to_string())?;
    let StickerSet {
        name: set_name,
        title,
        stickers,
    } = set;
    let pairs: Vec<(String, String)> = stickers
        .into_iter()
        .filter(|s| !s.file_id.is_empty())
        .map(|s| (s.emoji.unwrap_or_default(), s.file_id))
        .collect();
    if pairs.is_empty() {
        return Err(format!("贴纸包 {name} 里没有取到任何贴纸"));
    }
    // 服务端给的 name 是权威值；拿不到就退回入参，总得有个 set 字段可读。
    let set_name = if set_name.trim().is_empty() {
        name.to_string()
    } else {
        set_name
    };
    Ok(crate::core::sticker::StickerCatalog::from_set(
        &set_name,
        title.trim(),
        &pairs,
    ))
}

/// 用户发来消息时解析/确认会话映射，返回会话 id（映射建不起来则 None）。
///
/// 主动开口的静默窗口不再由状态表维护——它直接取 `messages` 里最后一条用户消息的
/// 真实发出时刻，所以这里只剩"确保映射存在"这一件事（否则该用户进不了候选集）；
/// session id 顺手交给调用方，后续落库与起轮次都要用。
fn touch_user_reply(
    session: &Sessions,
    external_id: &str,
    _sent_at: Option<DateTime<Utc>>,
) -> Option<String> {
    match session.resolve(CHANNEL, external_id) {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(error = %e, "解析会话映射失败");
            None
        }
    }
}

/// 从一条入站消息里取出交给模型的文本。
///
/// Telegram 把非文本消息放进各自的字段（`sticker` / `photo` / `voice` …），没有
/// `text`。目前只认**贴纸**：转成一句可读的占位描述，让模型知道"对方发来一张
/// 表情包"，而不是整条被丢掉。其余非文本类型暂不处理——返回 None 即忽略。
/// 这跟出站侧 [`Reply::Sticker`] 的投影是对称的。
fn inbound_text(message: &TgMessage) -> Option<String> {
    if let Some(text) = message.text.as_deref() {
        if !text.trim().is_empty() {
            return Some(text.to_string());
        }
    }
    let emoji = message
        .sticker
        .as_ref()
        .and_then(|s| s.emoji.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match emoji {
        Some(e) => Some(format!("[对方发来一张表情包 {e}]")),
        None if message.sticker.is_some() => Some("[对方发来一张表情包]".to_string()),
        None => None,
    }
}

/// 与 [`inbound_text`] 同构的"本条消息说了什么"，但把语音/音频也纳进来。
///
/// 拆成"先解析、后干活"两步是为了满足 FR-003：**过期**的语音必须能被识别出来并直接
/// 存档，绝不能先付一遍下载 + 转写的代价才发现它根本不会被回复（见 `run_loop`）。
/// 本条消息的内容来源。生命周期与 [`TgMessage`] 的借用绑定（音频字段是借来的）。
enum Inbound<'a> {
    /// 直接可用的文本（含贴纸占位描述）。
    Text(String),
    /// 需要转写的音频附件。
    Audio(AudioRef<'a>),
}

/// 转写结果的分流。**判据是 HTTP 状态码**（契约见
/// `specs/001-telegram-voice-transcription/contracts/transcribe-api.md`）。
///
/// 分界线一句话：4xx 是"这条音频的问题"（交给模型用人的方式回应），
/// 5xx / 连接失败 / 超时是"我们自己的问题"（是缺陷，不许伪装成"没听清"）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum VoiceOutcome {
    /// 转写成功。`text` 已去首尾空白且非空。
    Transcribed { text: String },
    /// 音频里没听出内容（含整条无人声）。交模型自然回应（FR-004/005）。
    Unintelligible,
    /// 这条音频本身的问题：解不开、太长、不是音频。同样交模型自然回应（FR-003/005）。
    Rejected { reason: String },
    /// 我们自己的问题：能力未启用 / 连不上 / 超时 / 网关 5xx。
    /// **不落库、不起轮次**，只记错误（FR-006/013）。
    Skipped { reason: String },
}

/// 从 `/transcribe` 的 200 响应体里取 `text`。形状不对按系统故障处理（契约被违反了）。
fn transcribe_text(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    value
        .get("text")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| "响应缺少 text 字段".to_string())
}

/// 从 4xx/5xx 响应体里取 `error` 文本（取不到就退化成原文），只用于日志。
fn api_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string())
}

/// "什么都没听出来"的判据：空白、或只剩标点/符号。
///
/// ⚠️ **不**把简短应答（"嗯"、"好"）算进来——那是正常转写，该交给模型理解，而不是
/// 被当成"没听清"（见 spec 边界情形）。故判据是"有没有字母/数字/表意文字"，而不是
/// "有多长"。
fn heard_nothing(text: &str) -> bool {
    !text.chars().any(char::is_alphanumeric)
}

/// 网关 HTTP 状态码 + 响应体 → 分流结果。纯函数，便于单测。
fn voice_outcome(status: u16, body: &str) -> VoiceOutcome {
    match status {
        200 => match transcribe_text(body) {
            Ok(text) if !heard_nothing(&text) => VoiceOutcome::Transcribed { text },
            Ok(_) => VoiceOutcome::Unintelligible,
            Err(e) => VoiceOutcome::Skipped {
                reason: format!("转写响应无法解析（网关契约被破坏）：{e}"),
            },
        },
        422 => VoiceOutcome::Unintelligible,
        400 | 413 | 415 => VoiceOutcome::Rejected {
            reason: api_error(body),
        },
        // 500 / 503 以及任何没见过的状态码：宁可当成"我们自己的问题"（不落库、不编话术），
        // 也不要猜。状态码集合是封闭的，出现意外值本身就该被看见。
        _ => VoiceOutcome::Skipped {
            reason: format!("网关返回 {status}: {}", api_error(body)),
        },
    }
}

/// caption（用户自己写的说明，若有）在前，正文在后。
///
/// caption MUST NOT 被覆盖或丢弃（FR-012）；只有空白的 caption 等于没有。
fn with_caption(caption: Option<&str>, body: &str) -> String {
    match caption.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => format!("{c}\n{body}"),
        None => body.to_string(),
    }
}

/// 组装转写成功的落库内容：`[语音] <文本>`（caption 若有则在前面）。
fn transcribed_content(caption: Option<&str>, text: &str) -> String {
    with_caption(caption, &format!("{VOICE_PREFIX}{}", text.trim()))
}

/// caption（若有）在前、标记在后的落库内容。与 [`transcribed_content`] 同形，
/// 区别只是"正文"换成了情境描述标记。
fn marker_content(caption: Option<&str>, marker: &str) -> String {
    with_caption(caption, marker)
}

/// 入站语音转写的运行参数：从 [`Config`] 取一次快照。
///
/// 快照而不是每次现读，是为了让 `inbound_audio_content` 的分流规则可以被单测**注入**，
/// 不必去改进程环境变量（那是全局状态，测试间会互相污染）。
#[derive(Debug, Clone, Copy)]
struct VoiceCfg {
    enabled: bool,
    /// 单次转写硬超时（秒）。
    timeout: u64,
    /// 语音时长前置上限（秒）。
    max_secs: u64,
}

impl VoiceCfg {
    fn from_env() -> Self {
        let c = Config::get();
        Self {
            enabled: c.voice_transcribe_enabled,
            timeout: c.voice_transcribe_timeout,
            max_secs: c.voice_transcribe_max_secs,
        }
    }
}

/// 音频的处理决策：**在发起下载之前**就能定下来的部分。
#[derive(Debug, Clone, PartialEq, Eq)]
enum VoicePlan {
    /// 前置拦截：不下载、不转写，直接按内容层面失败落标记。
    Reject(&'static str),
    /// 值得下载并转写。
    Transcribe,
}

/// 前置拦截（FR-003）：这条音频值不值得付下载 + 转写的代价。
///
/// 两道闸：`duration` 超过 `max_secs`（默认 120s）、`file_size` 超过 20 MB。
/// 两者都归 `Rejected`，用同一个标记——对用户与模型来说"太大了"是同一件事。
///
/// ⚠️ 必须排在下载之前。缺少这道的后果很具体：一条几分钟的语音会一路跑到转写**超时**，
/// 用户长时间什么都收不到、最后还静默失败——正是 FR-003 禁止的"长时间无响应"
/// （research.md D10、H1）。字段缺失（`None`）时**不拦**：不知道就别猜，交给下载与
/// 网关去判。
fn voice_plan(audio: &AudioRef<'_>, max_secs: u64) -> VoicePlan {
    let too_long = audio
        .duration
        .is_some_and(|d| d > i64::try_from(max_secs).unwrap_or(i64::MAX));
    let too_big = audio.file_size.is_some_and(|s| s > MAX_VOICE_BYTES);
    if too_long || too_big {
        VoicePlan::Reject(VOICE_TOO_LARGE_MARKER)
    } else {
        VoicePlan::Transcribe
    }
}

impl TgMessage {
    /// 本条消息的"内容来源"：文本/贴纸优先，其次音频附件，都没有则 `None`（忽略）。
    ///
    /// 与 [`inbound_text`] 的优先级一致：文本 > 贴纸 > 语音 > 音频。
    fn resolve_inbound(&self) -> Option<Inbound<'_>> {
        if let Some(text) = inbound_text(self) {
            return Some(Inbound::Text(text));
        }
        // `inbound_text` 对语音/音频返回 None（它们没有 text 也没有 sticker），
        // 所以走到这里才轮到音频附件——顺序天然正确。
        self.audio_attachment().map(Inbound::Audio)
    }
}

/// "把一条音频送去转写"的抽象：返回 `(网关状态码, 响应体)`。
///
/// 存在的理由是**可测**。转写是本特性唯一的副作用入口（下载 + 调网关），把它收成一个
/// 窄接口后：① `inbound_audio_content` 的全部分流规则（成功 / 没听清 / 太大 / 系统故障）
/// 都能被单测覆盖；② "超限语音不会发起下载"（FR-003）能靠**调用次数**直接断言，
/// 而不是靠人读代码相信。
trait Transcriber {
    fn transcribe(
        &self,
        file_id: &str,
        mime: &str,
        timeout: u64,
    ) -> impl std::future::Future<Output = Result<(u16, String), String>> + Send;
}

/// 生产实现：真的去 Telegram 取文件、再送本机网关。
struct GatewayTranscriber<'a> {
    client: &'a reqwest::Client,
    token: &'a str,
    gateway_url: &'a str,
}

impl Transcriber for GatewayTranscriber<'_> {
    async fn transcribe(
        &self,
        file_id: &str,
        mime: &str,
        timeout: u64,
    ) -> Result<(u16, String), String> {
        transcribe_voice(
            self.client,
            self.token,
            self.gateway_url,
            timeout,
            file_id,
            mime,
        )
        .await
    }
}

/// 处理一条**新鲜**的入站音频：前置拦截 → 下载 → 转写 → 组装落库内容。
///
/// 返回 `None` 表示**不落库、不起轮次**（只可能是系统故障路径，FR-006/013）；
/// 返回 `Some(文本)` 表示照常落库并起轮次——与用户直接打字走**完全相同**的那条路。
///
/// 三个分支对应 spec 的三类结果，别混：
/// - 成功 → `[语音] <转写文本>`；
/// - 内容层面失败（没听出内容 / 这条音频解不开 / 太大）→ 情境描述标记，**照常起轮次**，
///   让模型自己用自然语言回应（FR-004/005）；
/// - 系统故障（能力没开 / 连不上 / 超时 / 网关 5xx）→ 什么都不落，只记 error。
async fn inbound_audio_content<T: Transcriber>(
    cfg: &VoiceCfg,
    chat_id: i64,
    audio: &AudioRef<'_>,
    caption: Option<&str>,
    transcriber: &T,
) -> Option<String> {
    if !cfg.enabled {
        tracing::error!(
            chat_id,
            "收到语音，但语音转写未启用（VOICE_TRANSCRIBE_ENABLED=0）：不落库不回复（FR-013）"
        );
        return None;
    }

    tracing::info!(
        chat_id,
        mime = audio.mime(),
        duration = ?audio.duration,
        file_size = ?audio.file_size,
        file_name = ?audio.file_name,
        title = ?audio.title,
        "开始转写入站语音"
    );

    // 【前置拦截】排在下载之前：这条音频如果注定处理不了，就**一点都不做**，直接按内容
    // 层面失败交给模型（FR-003）。这是"长语音不会长时间无响应"的落点。
    if let VoicePlan::Reject(marker) = voice_plan(audio, cfg.max_secs) {
        tracing::info!(
            chat_id,
            duration = ?audio.duration,
            file_size = ?audio.file_size,
            max_secs = cfg.max_secs,
            "语音超出处理上限，前置拦截：不下载、不转写（FR-003）"
        );
        return Some(marker_content(caption, marker));
    }

    let (status, body) = match transcriber
        .transcribe(audio.file_id, audio.mime(), cfg.timeout)
        .await
    {
        Ok(pair) => pair,
        // 下载失败 / 连不上网关 / 读超时：都是我们这边的问题（FR-006）。
        Err(e) => {
            tracing::error!(
                error = %e,
                chat_id,
                "语音转写调用失败（系统故障）：不落库不回复（FR-006/013）"
            );
            return None;
        }
    };

    match voice_outcome(status, &body) {
        VoiceOutcome::Transcribed { text } => {
            tracing::info!(chat_id, chars = text.chars().count(), "语音转写成功");
            Some(transcribed_content(caption, &text))
        }
        VoiceOutcome::Unintelligible => {
            tracing::info!(chat_id, "语音里没听出内容，交模型自然回应（FR-004/005）");
            Some(marker_content(caption, VOICE_UNINTELLIGIBLE_MARKER))
        }
        VoiceOutcome::Rejected { reason } => {
            tracing::info!(
                chat_id,
                %reason,
                "语音内容层面失败，交模型自然回应（FR-004/005）"
            );
            Some(marker_content(caption, VOICE_UNINTELLIGIBLE_MARKER))
        }
        VoiceOutcome::Skipped { reason } => {
            tracing::error!(
                chat_id,
                %reason,
                "语音转写不可用（系统故障）：不落库、不起轮次、不产生任何面向用户的文本（FR-006/013）"
            );
            None
        }
    }
}

/// `/health` 里我们关心的 ASR 部分。
#[derive(Debug)]
struct AsrHealth {
    engine: String,
    importable: bool,
    model_loaded: bool,
}

/// 从 `/health` 响应体里取 ASR 摘要。字段缺失一律按"不知道 / 不可用"处理，**绝不 panic**——
/// 探测是锦上添花，不该有自己的失败模式。
fn asr_health(value: &serde_json::Value) -> AsrHealth {
    let asr = value.get("asr");
    let pick = |k: &str| asr.and_then(|a| a.get(k));
    AsrHealth {
        engine: pick("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("（网关未报告）")
            .to_string(),
        importable: pick("importable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        model_loaded: pick("model_loaded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    }
}

/// 启动自检（FR-013）：把转写能力的**真实**状态打进日志。
///
/// 目的是省掉"发了语音没反应，还得去翻代码猜是哪一层"这种排错。两种情况都要有明确日志：
/// 未启用时直说未启用；启用时报告网关的引擎 / 依赖 / 模型状态。
///
/// **任何失败都只记 warn，不上抛、不阻止长轮询**——网关是独立进程，没起是常态。
async fn probe_voice_capability(cfg: &VoiceCfg, gateway_url: &str) {
    if !cfg.enabled {
        tracing::info!(
            "入站语音转写未启用（VOICE_TRANSCRIBE_ENABLED=0）：收到语音只记 error，不落库不回复"
        );
        return;
    }
    let client = match reqwest::Client::builder()
        .no_proxy() // 理由同 media_generate：本机代理会把 127.0.0.1 也路由进去
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "语音能力自检跳过：创建探测客户端失败");
            return;
        }
    };
    match client.get(format!("{gateway_url}/health")).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(value) => {
                let h = asr_health(&value);
                if h.importable {
                    tracing::info!(
                        engine = %h.engine,
                        importable = h.importable,
                        model_loaded = h.model_loaded,
                        "语音能力自检通过（转写可用）"
                    );
                } else {
                    tracing::warn!(
                        engine = %h.engine,
                        "语音能力自检：网关报告 ASR 不可用，转写会返回 503——检查 media-gateway 的 \
                         ASR_ENGINE 与 faster-whisper 安装（见 gateway/asr/）"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "语音能力自检：/health 响应不是合法 JSON"),
        },
        Ok(resp) => tracing::warn!(
            status = resp.status().as_u16(),
            gateway_url,
            "语音能力自检：网关 /health 返回非成功状态"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            gateway_url,
            "语音能力自检：连不上媒体网关（未启动？）；长轮询照常开始"
        ),
    }
}

/// 命令类消息的即时回执：`/start` 打招呼、`/new` 重开会话。
///
/// 返回 `Some(文本)` 表示这是命令、已处理完，直接把这段文本发出去即可；`None` 表示普通
/// 消息，交给 AI 流水线。命令不进 AI 流水线，所以既不必起投递工人，也不必占一个阻塞线程。
fn command_reply(session: &Sessions, chat_id: i64, text: &str) -> Result<Option<String>, String> {
    match text.trim() {
        "/start" => Ok(Some(
            "你好，我是你的记忆助手。直接发消息聊天，我会记住值得记住的事；发送 /new 开始一段新的对话。"
                .to_string(),
        )),
        "/new" => {
            session
                .reset(CHANNEL, &chat_id.to_string())
                .map_err(|e| e.to_string())?;
            Ok(Some("已开始新对话，之前的记录保留在记忆库里。".to_string()))
        }
        _ => Ok(None),
    }
}

/// 处理一条 TG 对话消息，返回本轮对话结果。
///
/// 用户消息**在通道侧收到时就已落库**（见 [`run_loop`]），这里按 id 复用、不再重复入库：
/// 落库先于起轮次，是"运行中插话"成立的前提——一轮在跑时新到的消息先落库、由那一轮在
/// 迭代边界取用（见 `core::agent::ingest_steers`），而不是排队等这轮跑完。
///
/// 回复在**本轮内即时投递**（见 [`TgDeliver`]）：内核在工具循环里逐片段投递，骰子点数
/// 也在这条路径上拿回并回填给模型。只负责产出回复；记忆由模型在本轮内自行决定是否写入
/// （`remember` 工具）。命令类消息（`/start` / `/new`）不走到这里，见 [`command_reply`]。
#[allow(clippy::too_many_arguments)]
async fn handle_text(
    chat: Arc<Agent>,
    session: Arc<Sessions>,
    chat_id: i64,
    text: &str,
    user_time: Option<String>,
    deliverer: TgDeliver,
    user_message_id: String,
) -> Result<ChatOutput, String> {
    let session_id = session
        .resolve(CHANNEL, &chat_id.to_string())
        .map_err(|e| e.to_string())?;
    let text = text.to_string();
    // 同步的对话流水线要在阻塞线程池里跑（api/chat.rs 同理）；投递口一并移进去，
    // 内核在工具循环里即时发送、当场拿回骰子点数。投递要阻塞等回执，只有在这个
    // 阻塞线程上才安全——放到 runtime 工人线程上会把调度线程占死。
    let worker = deliverer.clone();
    tokio::task::spawn_blocking(move || {
        let input = ChatInput {
            message: text,
            session_id: Some(session_id),
            history: Vec::new(),
            history_count: None,
            user_time,
        };
        // 槽位形参只服务于 Web/iLink 那条"由 `chat_stream` 自行入库"的老路，Telegram
        // 这边消息已落库、id 由调用方直接持有，传空槽即可。
        chat.chat_stream(
            &input,
            move |reply| worker.deliver(reply),
            std::sync::Arc::new(std::sync::Mutex::new(None)),
            Some(user_message_id),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("对话任务失败: {e}"))?
}

/// 起一轮对话：投到独立任务里跑，**长轮询不等它**。
///
/// 这是"agent 跑着也能插话"在通道侧的另一半：轮询循环必须一直转着去收新消息，否则用户
/// 的第二句话要等这一轮跑完、长轮询被重新调用才拿得到。轮次名额由凭证 [`TurnTicket`]
/// 登记，同一会话同时只跑一轮（[`Sessions::begin_turn`]）；期间新到的消息照常落库，
/// 由这一轮在迭代边界并进上下文（见 `core::agent` 的插话机制）。
///
/// 失败/超时的处理与过去一致：一条都没送达才入队静默重试（见 [`schedule_retry`]）；
/// 已经有内容送达就不再重跑（重跑会把同样的话再说一遍）。后一种情况下，本轮没赶上的
/// 插话不会丢——它就在时间线里，用户下一条消息进来时一并被看到。
#[allow(clippy::too_many_arguments)] // 会话、连接、消息、凭证各占一个，都是起一轮必需的输入
fn spawn_turn(
    chat: Arc<Agent>,
    session: Arc<Sessions>,
    client: reqwest::Client,
    base: String,
    chat_id: i64,
    session_id: String,
    text: String,
    user_time: Option<String>,
    user_message_id: String,
    ticket: TurnTicket,
) {
    tokio::spawn(async move {
        // 凭证必须在任务内持有到结束（含超时）：任务一结束它就析构，会话随之放行下一轮。
        let _ticket = ticket;
        // 本轮产出的是**一组独立消息**（模型自己决定说几句），由内核在工具循环里
        // 逐条即时投递：投递口把片段交给同一 runtime 上的工人去发，内核对每个片段
        // 阻塞等回执，顺序天然不乱，骰子点数也在这条路径上当场拿回并回填给模型。
        let (deliver_tx, deliver_rx) = tokio::sync::mpsc::unbounded_channel();
        let deliverer = TgDeliver::new(deliver_tx);
        let worker = tokio::spawn(deliver_worker(
            client,
            base,
            chat_id,
            Config::get().media_gateway_url.clone(),
            Config::get().media_gateway_timeout,
            deliver_rx,
        ));
        let started = std::time::Instant::now();

        let handled = tokio::time::timeout(
            Duration::from_secs(super::AI_DEADLINE_SECS),
            handle_text(
                chat,
                session.clone(),
                chat_id,
                &text,
                user_time.clone(),
                deliverer.clone(),
                user_message_id.clone(),
            ),
        )
        .await;

        match handled {
            Ok(Ok(out)) => {
                tracing::info!(
                    chat_id,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    messages = out.messages.len(),
                    memory_written = out.memory.is_some(),
                    "handle_text 返回（回复已即时投递）"
                );
            }
            Ok(Err(e)) => {
                let delivered = deliverer.delivered_count();
                if should_retry_after_failure(delivered) {
                    tracing::error!(
                        error = %e,
                        chat_id,
                        "对话失败，入队待重试（不再向用户发声）"
                    );
                    schedule_retry(
                        &session,
                        &session_id,
                        chat_id,
                        &text,
                        user_time,
                        &user_message_id,
                        false,
                    );
                } else {
                    tracing::warn!(
                        error = %e,
                        chat_id,
                        delivered,
                        "对话失败，但本轮已有内容送达：不重试（重跑会把同样的话再说一遍）"
                    );
                }
            }
            Err(_) => {
                let delivered = deliverer.delivered_count();
                if should_retry_after_failure(delivered) {
                    tracing::warn!(
                        chat_id,
                        deadline_secs = super::AI_DEADLINE_SECS,
                        "单轮对话超时，入队待重试（不再向用户发声）"
                    );
                    schedule_retry(
                        &session,
                        &session_id,
                        chat_id,
                        &text,
                        user_time,
                        &user_message_id,
                        true,
                    );
                } else {
                    tracing::warn!(
                        chat_id,
                        delivered,
                        deadline_secs = super::AI_DEADLINE_SECS,
                        "单轮对话超时，但本轮已有内容送达：不重试（重跑会把同样的话再说一遍）"
                    );
                }
            }
        }

        // 收尾：丢弃投递口让工人自然退出，再兜底 abort（超时那轮的内核任务可能
        // 还挂着，不能再等它，也不能让工人一直空转）。**不 await 工人**——超时后
        // 那个仍存活的 `spawn_blocking` 会一直持有它的发送端。
        drop(deliverer);
        worker.abort();
    });
}

/// 长轮询入口。token 为空时直接返回，由 server 入口决定是否调用。
/// 异常退出后自动重连（启动期网络抖动不再导致永久掉线）。
pub async fn run(chat: Arc<Agent>, session: Arc<Sessions>) {
    let token = Config::get().telegram_bot_token.trim().to_string();
    if token.is_empty() {
        return;
    }
    loop {
        match run_loop(chat.clone(), session.clone(), &token).await {
            Ok(()) => break,
            Err(e) => {
                tracing::error!(error = %e, "telegram 轮询异常退出，60s 后重连");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }
}

async fn run_loop(chat: Arc<Agent>, session: Arc<Sessions>, token: &str) -> Result<(), String> {
    let client = build_client()?;
    let base = api_base(token);

    // 校验 token。
    let me: ApiResponse<serde_json::Value> = api_get(&client, &format!("{base}/getMe")).await?;
    if !me.ok {
        return Err(format!(
            "getMe 失败：{}",
            me.description.unwrap_or_default()
        ));
    }
    tracing::info!("telegram bot 已连接，开始长轮询");
    super::shared_push()
        .telegram_ready
        .store(true, std::sync::atomic::Ordering::Relaxed);

    // 入站语音转写：运行参数取一次快照，并做一次能力自检。
    // 自检**只影响日志**——网关没起、依赖没装都不该阻止长轮询（FR-013）。
    let voice = VoiceCfg::from_env();
    let gateway_url = Config::get().media_gateway_url.clone();
    probe_voice_capability(&voice, &gateway_url).await;

    // 拉取游标：从上次确认的位置续传，而不是跳到最新把离线期消息整段丢弃。
    //
    // Telegram 服务端会保留 update 约 24 小时，进程重启后仍能取回这期间的消息。
    // 取回后按消息真实时刻做时效分级（见 run_loop）：太旧的只入库不回复，
    // 有延迟的交给模型自己决定怎么接。用户因此不必为"上次没被收到"重发一遍。
    //
    // 首次运行（无游标）从 0 开始，即领取服务端保留的全部历史——此时几乎必然
    // 全是超时效的旧消息，会被 archive 分支挡掉，不产生任何回复。
    let mut offset: i64 = match session
        .load_cursor(CHANNEL)
        .as_deref()
        .and_then(|s| s.parse().ok())
    {
        Some(saved) => {
            tracing::info!(offset = saved, "telegram 续传游标，从上次位置继续");
            saved
        }
        None => {
            tracing::info!("telegram 无历史游标，从 0 开始领取服务端保留的 update");
            0
        }
    };

    let allowed = Config::get().telegram_allowed_ids.clone();

    loop {
        let url = format!(
            "{base}/getUpdates?timeout={POLL_TIMEOUT_SECS}&offset={offset}&allowed_updates=%5B%22message%22%5D"
        );
        let updates: ApiResponse<Vec<Update>> = match api_get(&client, &url).await {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, "getUpdates 失败，{RETRY_DELAY_SECS}s 后重试");
                tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)).await;
                continue;
            }
        };
        if !updates.ok {
            tracing::warn!(
                description = ?updates.description,
                "getUpdates 返回 ok=false，重试"
            );
            tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)).await;
            continue;
        }

        let batch = updates.result.unwrap_or_default();
        if batch.is_empty() {
            // 长轮询正常空转，不刷日志（进程存活与否由 watchdog 探针单独确认）。
            continue;
        }
        tracing::info!(updates = batch.len(), "getUpdates 轮询返回");

        // 整批确认后立即落盘游标：即使后续处理中途崩溃，重启也不会重放这批。
        offset = offset.max(
            batch
                .iter()
                .map(|u| u.update_id + 1)
                .max()
                .unwrap_or(offset),
        );
        session.save_cursor(CHANNEL, &offset.to_string());

        for update in batch {
            offset = offset.max(update.update_id + 1);
            let Some(message) = update.message else {
                continue;
            };
            let chat_id = message.chat.id;
            if !allowed.is_empty() && !allowed.contains(&chat_id) {
                tracing::info!(chat_id, "未在白名单，忽略");
                continue;
            }
            let user_time = super::ts_to_rfc3339(message.date);
            let sent_at = user_time.as_deref().and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            });

            // 时效分级：离线积压的旧消息不该被当成刚收到的实时消息来回。
            //
            // 【前移】这一步排在**任何下载/转写之前**：过期语音不该为它永远不会被回复的
            // 结局付一遍 ASR 成本。原先它排在内容解析之后，语音因此没法先被识别出来。
            let freshness = sent_at
                .map(|at| classify_inbound(at, Utc::now()))
                .unwrap_or(InboundFreshness::Fresh);

            // 本条消息的内容来源（文本 / 贴纸 / 音频附件）；都不是则整条忽略（既有行为）。
            let Some(content) = message.resolve_inbound() else {
                continue;
            };

            let text = match content {
                // 过期的文本/贴纸：存档不回复（行为与改动前完全一致）。
                Inbound::Text(text) if freshness == InboundFreshness::TooOld => {
                    tracing::info!(chat_id, "消息已超时效，只入库不回复");
                    let _ = touch_user_reply(&session, &chat_id.to_string(), sent_at);
                    if let Err(e) = session.archive_only(
                        CHANNEL,
                        &chat_id.to_string(),
                        &text,
                        sent_at.unwrap_or_else(Utc::now),
                    ) {
                        tracing::warn!(error = %e, chat_id, "过期消息入库失败");
                    }
                    continue;
                }
                // 过期语音：只落一条占位说明，**不下载、不转写**。
                Inbound::Audio(_audio) if freshness == InboundFreshness::TooOld => {
                    tracing::info!(chat_id, "语音已超时效，只入库不转写");
                    let _ = touch_user_reply(&session, &chat_id.to_string(), sent_at);
                    if let Err(e) = session.archive_only(
                        CHANNEL,
                        &chat_id.to_string(),
                        EXPIRED_VOICE_PLACEHOLDER,
                        sent_at.unwrap_or_else(Utc::now),
                    ) {
                        tracing::warn!(error = %e, chat_id, "过期语音入库失败");
                    }
                    continue;
                }
                Inbound::Text(text) => text,
                // 新鲜语音/音频：转成文字，之后与用户直接打字走**完全相同**的那条路。
                Inbound::Audio(audio) => {
                    let caption = message.caption.as_deref();
                    let transcriber = GatewayTranscriber {
                        client: &client,
                        token,
                        gateway_url: &gateway_url,
                    };
                    match inbound_audio_content(&voice, chat_id, &audio, caption, &transcriber)
                        .await
                    {
                        Some(text) => text,
                        // 系统故障：不落库、不起轮次（FR-006/013）。
                        None => continue,
                    }
                }
            };

            tracing::info!(
                chat_id,
                ?freshness,
                "收到消息，开始处理（进入 AI 流水线前）"
            );

            // 会话映射先建起来：下面的入库、起轮次都要 session id，建不起来这轮就没法进行。
            let Some(session_id) = touch_user_reply(&session, &chat_id.to_string(), sent_at) else {
                continue;
            };

            // 命令类消息（`/start` / `/new`）在异步侧直接回执，不进 AI 流水线，
            // 也就不必起投递工人与阻塞线程。
            match command_reply(&session, chat_id, &text) {
                Ok(Some(reply)) => {
                    send_typing(&client, &base, chat_id).await;
                    if let Err(e) = send_text(&client, &base, chat_id, &reply).await {
                        tracing::error!(error = %e, chat_id, "命令回执发送失败");
                    }
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(error = %e, chat_id, "命令处理失败");
                    continue;
                }
            }

            // 用户消息**落库先于起轮次**——这是"运行中插话"的前提：一轮在跑时新到的消息
            // 不再排队等它跑完，而是先躺在时间线上，由那一轮在迭代边界取用（`core::agent`
            // 的插话机制）。落库用报文里的真实发出时刻，时间线不受处理延迟影响。
            let persisted = match sent_at {
                Some(at) => session.add_message_at(&session_id, "user", &text, at),
                None => session.add_message(&session_id, "user", &text),
            };
            let user_message_id = match persisted {
                Ok(m) => m.id,
                Err(e) => {
                    tracing::error!(error = %e, chat_id, "用户消息入库失败，本轮作罢（不入库不回）");
                    continue;
                }
            };

            send_typing(&client, &base, chat_id).await;

            // 同一会话同时只跑一轮：已有轮次在跑时**不再另起一轮**，刚落的这条消息由
            // 那一轮在迭代边界取用；取不上（这轮已收尾）也仍在时间线里，下次对话照读得到。
            let Some(ticket) = session.begin_turn(&session_id) else {
                tracing::info!(
                    chat_id,
                    "本会话已有轮次在跑，消息入库后交给它，不再另起一轮"
                );
                continue;
            };

            spawn_turn(
                chat.clone(),
                session.clone(),
                client.clone(),
                base.clone(),
                chat_id,
                session_id,
                text,
                user_time,
                user_message_id,
                ticket,
            );
        }
    }
}

/// 失败轮次重试扫表：周期性扫描 `pending_retry`，把到期且未达上限的轮次重新跑一遍。
///
/// - 复用已落库的用户消息（同一 `user_message_id`），不重复落库；
/// - **成功**才由工具循环经 [`TgDeliver`] 把回复送达用户，然后从队列删除；
/// - **失败**推进退避（写回 `next_attempt_at`）等下一轮；耗尽则静默放弃。
///
/// 无论成功/放弃，用户消息始终留在 `messages` 时间线，不丢上下文；失败也**不向用户发
/// 任何话术**（原"刚才走神了"会误导重发，而消息其实已落库）。
pub async fn run_retry_sweep(chat: Arc<Agent>, session: Arc<Sessions>) {
    let token = Config::get().telegram_bot_token.trim().to_string();
    if token.is_empty() {
        return;
    }
    let client = match telegram_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "重试扫表无法构建 Telegram client，退出");
            return;
        }
    };
    let base = api_base(&token);
    let media_url = Config::get().media_gateway_url.clone();
    let media_timeout = Config::get().media_gateway_timeout;

    let mut ticker = tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
    loop {
        ticker.tick().await;
        let now = Utc::now();
        let due = match session.due_retries(now) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "读取待重试队列失败，下一周期重试");
                continue;
            }
        };
        // 逐行顺序处理（每行 `await` 到结果），天然不会自行并发；跨轮次对同一行的重叠由
        // `next_attempt_at` 退避兜住——超时轮次的下次重试被推到 `RETRY_TIMEOUT_GRACE_SECS`
        // 之后（大于单次 LLM 调用上限），原阻塞任务此时已收尾，不会与新重试并发。
        for r in due {
            let chat_id = match r.chat_id.parse::<i64>() {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(error = %e, id = %r.chat_id, "pending_retry.chat_id 非法，跳过");
                    continue;
                }
            };
            // 会话里已经出现更新的用户消息 = 这条重试的目标过时了（用户后来又说了一句，
            // 那一轮自己会跑/已经跑了）。再重试只会答上一句旧话，还可能把新话重复回一遍，
            // 直接出队：用户消息仍在时间线里，上下文不丢。
            match session.newer_user_message(&r.session_id, &r.user_message_id) {
                Ok(Some(_)) => {
                    tracing::info!(chat_id, "会话里已有更新的用户消息，放弃这条过时重试");
                    let _ = session.remove_retry(&r.id);
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, chat_id, "读取会话最新消息失败，仍按原目标重试")
                }
            }

            // 会话正跑着轮次时不并跑：本轮扫表跳过它，下一周期再看（对方那一轮很可能
            // 已经把这句回掉了，届时会被上面的"过时"判据出队）。
            let Some(ticket) = session.begin_turn(&r.session_id) else {
                tracing::info!(chat_id, "本会话已有轮次在跑，重试推迟到下一周期");
                continue;
            };
            // 凭证持到本行处理结束，让插话机制（同会话新消息并进本轮上下文）照常生效。
            let _ticket = ticket;

            // 同一次扫表内顺序处理，每个轮次用独立投递工人即时送达回复。
            let (deliver_tx, deliver_rx) = tokio::sync::mpsc::unbounded_channel();
            let deliverer = TgDeliver::new(deliver_tx);
            let worker = tokio::spawn(deliver_worker(
                client.clone(),
                base.clone(),
                chat_id,
                media_url.clone(),
                media_timeout,
                deliver_rx,
            ));
            let handled = tokio::time::timeout(
                Duration::from_secs(super::AI_DEADLINE_SECS),
                handle_text(
                    chat.clone(),
                    session.clone(),
                    chat_id,
                    &r.content,
                    r.user_time.clone(),
                    deliverer.clone(),
                    // 重试目标消息早已落库，按 id 复用同一行，不重复入库。
                    r.user_message_id.clone(),
                ),
            )
            .await;
            // 收尾：丢弃投递口、abort 工人（超时那轮的内核任务可能仍挂着，不 await）。
            // 失败分支要按"本轮已送达几片"决定还重不重试，先把计数取出来再丢投递口。
            let delivered = deliverer.delivered_count();
            drop(deliverer);
            worker.abort();

            match handled {
                Ok(Ok(_)) => {
                    if let Err(e) = session.remove_retry(&r.id) {
                        tracing::error!(error = %e, chat_id, "重试成功但删除队列项失败");
                    } else {
                        tracing::info!(chat_id, "待重试轮次重试成功，回复已送达，出队");
                    }
                }
                Ok(Err(e)) => {
                    if !should_retry_after_failure(delivered) {
                        // 这次重试已经把一部分话说出去了：再跑一遍就是重复同样的内容，收手出队。
                        tracing::warn!(
                            error = %e,
                            chat_id,
                            delivered,
                            "重试中途失败，但已有内容送达：不再重试，出队"
                        );
                        let _ = session.remove_retry(&r.id);
                    } else {
                        let attempts = r.attempts + 1;
                        if attempts >= MAX_RETRY_ATTEMPTS {
                            tracing::warn!(
                                error = %e,
                                chat_id,
                                attempts,
                                "重试仍失败且已达上限，静默放弃（用户消息保留在时间线）"
                            );
                            let _ = session.remove_retry(&r.id);
                        } else {
                            let next = next_backoff(attempts, Utc::now(), false);
                            if let Err(me) = session.mark_retry_attempted(&r.id, next) {
                                tracing::error!(error = %me, chat_id, "推进重试退避失败");
                            } else {
                                tracing::warn!(
                                    error = %e,
                                    chat_id,
                                    attempts,
                                    "重试仍失败，已退避，等待下一轮扫表"
                                );
                            }
                        }
                    }
                }
                Err(_) => {
                    if !should_retry_after_failure(delivered) {
                        tracing::warn!(
                            chat_id,
                            delivered,
                            "重试超时，但已有内容送达：不再重试，出队"
                        );
                        let _ = session.remove_retry(&r.id);
                    } else {
                        let attempts = r.attempts + 1;
                        if attempts >= MAX_RETRY_ATTEMPTS {
                            tracing::warn!(
                                chat_id,
                                attempts,
                                "重试超时且已达上限，静默放弃（用户消息保留在时间线）"
                            );
                            let _ = session.remove_retry(&r.id);
                        } else {
                            // 超时：旧阻塞任务可能仍跑，下次重试时刻推到超时之后。
                            let next = next_backoff(attempts, Utc::now(), true);
                            if let Err(me) = session.mark_retry_attempted(&r.id, next) {
                                tracing::error!(error = %me, chat_id, "推进重试退避失败");
                            } else {
                                tracing::warn!(
                                    chat_id,
                                    attempts,
                                    "重试超时，已退避，等待下一轮扫表"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "telegram_tests.rs"]
mod telegram_tests;
