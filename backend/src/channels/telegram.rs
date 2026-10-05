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
use crate::core::session::{classify_inbound, InboundFreshness, Sessions};
use crate::{ChatInput, ChatOutput};

const CHANNEL: &str = "telegram";
/// Telegram 单条消息文本上限（字符）。
const MAX_MESSAGE_LEN: usize = 4096;
/// getUpdates 长轮询超时（秒）。
const POLL_TIMEOUT_SECS: u64 = 50;
/// 网络异常后的退避等待（秒）。
const RETRY_DELAY_SECS: u64 = 5;

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
    chat: TgChat,
    /// 消息发出时刻（Unix 秒，服务端收到即记），等于用户按下发送的世界时刻。
    #[serde(default)]
    date: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

#[derive(Debug, Serialize)]
struct SendMessage<'a> {
    chat_id: i64,
    text: &'a str,
}

/// `sendMessage` 响应体。Bot API 会用 200 携带业务错误（`ok:false`），
/// 限流时在 `parameters.retry_after` 里给出应当等待的秒数。
#[derive(Debug, Default, Deserialize)]
struct SendResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<SendParameters>,
}

#[derive(Debug, Deserialize)]
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

use super::chunk_text;
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
/// 每片独立重试：已送达的前片不会因后片最终失败而被重发（否则用户会收到重复内容）。
/// 只对"重投可能成功"的失败退避重试——传输层错误、429 限流、5xx、响应被截断；
/// 4xx 与 `ok:false` 属确定性拒绝，立即放弃，把失败交回调用方。
async fn send_text(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    text: &str,
) -> Result<(), String> {
    for chunk in chunk_text(text, MAX_MESSAGE_LEN) {
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

async fn send_typing(client: &reqwest::Client, base: &str, chat_id: i64) {
    let _ = client
        .post(format!("{base}/sendChatAction"))
        .timeout(Duration::from_secs(10))
        .json(&serde_json::json!({ "chat_id": chat_id, "action": "typing" }))
        .send()
        .await;
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

/// 用户发来消息时解析/确认会话映射。
///
/// 主动开口的静默窗口不再由状态表维护——它直接取 `messages` 里最后一条用户消息的
/// 真实发出时刻，所以这里只剩"确保映射存在"这一件事（否则该用户进不了候选集）。
fn touch_user_reply(session: &Sessions, external_id: &str, _sent_at: Option<DateTime<Utc>>) {
    if let Err(e) = session.resolve(CHANNEL, external_id) {
        tracing::warn!(error = %e, "解析会话映射失败");
    }
}

/// 模型整轮没说任何话、或本轮失败/超时时的兜底话术。
const FALLBACK_REPLY: &str = "刚才走神了，再发一次试试。";

/// 命令类回执：没有用户输入，`user_message_id` 留空表示"本轮无消息来源"。
fn plain_reply(reply: &str) -> ChatOutput {
    ChatOutput {
        session_id: String::new(),
        reply: reply.to_string(),
        messages: vec![reply.to_string()],
        user_message_id: String::new(),
        memory: None,
    }
}

/// 处理一条 TG 文本消息，返回本轮对话结果（None 表示无需回复）。
///
/// 只负责产出回复；记忆由模型在本轮内自行决定是否写入（`remember` 工具）。
async fn handle_text(
    chat: Arc<Agent>,
    session: Arc<Sessions>,
    chat_id: i64,
    text: &str,
    user_time: Option<String>,
) -> Result<Option<ChatOutput>, String> {
    let external = chat_id.to_string();
    let text = text.to_string();
    match text.trim() {
        "/start" => Ok(Some(plain_reply(
            "你好，我是你的记忆助手。直接发消息聊天，我会记住值得记住的事；发送 /new 开始一段新的对话。",
        ))),
        "/new" => {
            session
                .reset(CHANNEL, &external)
                .map_err(|e| e.to_string())?;
            Ok(Some(plain_reply("已开始新对话，之前的记录保留在记忆库里。")))
        }
        _ => {
            let session_id = session
                .resolve(CHANNEL, &external)
                .map_err(|e| e.to_string())?;
            // 同步的对话流水线要在阻塞线程池里跑（api/chat.rs 同理）。
            let mut out = tokio::task::spawn_blocking(move || {
                let input = ChatInput {
                    message: text.to_string(),
                    session_id: Some(session_id),
                    history: Vec::new(),
                    history_count: None,
                    user_time,
                };
                chat
                    .chat_stream(&input, |_| {})
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("对话任务失败: {e}"))??;
            // 模型可能整轮只调只读工具、没说任何话；`reply` 已被拼成空串。
            // 逐条发送用 `messages`，两者必须同步兜底，否则会出现"发了 (空回复)"
            // 而 `reply` 还是空的不一致状态。
            if out.messages.iter().all(|m| m.trim().is_empty()) {
                out.messages = vec![FALLBACK_REPLY.to_string()];
            }
            out.reply = out.messages.join("\n");
            Ok(Some(out))
        }
    }
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
        tracing::info!(updates = batch.len(), "getUpdates 轮询返回");
        if batch.is_empty() {
            // 长轮询正常空转：周期性地确认循环仍在呼吸（可作为心跳检查）。
            continue;
        }

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
            let Some(text) = message.text else { continue };
            if text.trim().is_empty() {
                continue;
            }
            let user_time = super::ts_to_rfc3339(message.date);
            let sent_at = user_time.as_deref().and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            });

            // 时效分级：离线积压的旧消息不该被当成刚收到的实时消息来回。
            let freshness = sent_at
                .map(|at| classify_inbound(at, Utc::now()))
                .unwrap_or(InboundFreshness::Fresh);
            if freshness == InboundFreshness::TooOld {
                tracing::info!(chat_id, "消息已超时效，只入库不回复");
                touch_user_reply(&session, &chat_id.to_string(), sent_at);
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

            tracing::info!(
                chat_id,
                ?freshness,
                "收到消息，开始处理（进入 AI 流水线前）"
            );

            touch_user_reply(&session, &chat_id.to_string(), sent_at);

            send_typing(&client, &base, chat_id).await;
            let handle_started = std::time::Instant::now();

            // 本轮产出的是**一组独立消息**（模型自己决定说几句），逐条发送。
            // 整段 join 成一首发出去等于退回单条长回复，整个改造就白做了。
            let (next_messages, is_fallback) = match tokio::time::timeout(
                Duration::from_secs(super::AI_DEADLINE_SECS),
                handle_text(chat.clone(), session.clone(), chat_id, &text, user_time),
            )
            .await
            {
                Ok(Ok(Some(out))) => {
                    tracing::info!(
                        chat_id,
                        elapsed_ms = handle_started.elapsed().as_millis() as u64,
                        messages = out.messages.len(),
                        memory_written = out.memory.is_some(),
                        "handle_text 返回（回复生产阶段完成）"
                    );
                    (out.messages, false)
                }
                Ok(Ok(None)) => continue,
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "对话失败");
                    (vec![FALLBACK_REPLY.to_string()], true)
                }
                Err(_) => {
                    tracing::warn!(
                        chat_id,
                        deadline_secs = super::AI_DEADLINE_SECS,
                        "单轮对话超时，放弃本轮，继续轮询"
                    );
                    (vec![FALLBACK_REPLY.to_string()], true)
                }
            };

            // 逐条投递：条与条之间留真人打字节奏，并在每次发送前刷新打字态
            //（工具轮不产消息时模型在想，首条之前可能已经等了很久）。
            let mut sent_any = false;
            for (i, one) in next_messages.iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(super::pacing_delay(i)).await;
                }
                send_typing(&client, &base, chat_id).await;
                match send_text(&client, &base, chat_id, one).await {
                    Ok(()) => sent_any = true,
                    Err(e) => {
                        tracing::error!(error = %e, chat_id, index = i, "回复发送失败");
                        break;
                    }
                }
            }

            if sent_any {
                tracing::info!(
                    chat_id,
                    is_fallback,
                    sent = next_messages.len(),
                    "回复已发送"
                );
            }
        }
    }
}
