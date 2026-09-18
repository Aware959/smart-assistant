//! Telegram Bot 接入（长轮询）。
//!
//! - 启动时用 `getMe` 校验 token，并丢弃历史积压 update；
//! - 每个 chat 映射独立 session（[`crate::db::channel`]），多轮上下文互不串扰；
//! - 文本超长时按 4096 字符分片发送；
//! - 代理优先级：`TELEGRAM_PROXY` → `HTTPS_PROXY` → `ALL_PROXY`
//!   （api.telegram.org 在部分地区不可直连）。

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::host::Host;
use crate::{Assistant, ChatInput, ChatOutput};

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
        builder = builder.proxy(
            reqwest::Proxy::all(&proxy).map_err(|e| format!("无效的代理地址: {e}"))?,
        );
    }
    builder.build().map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

use super::chunk_text;

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

async fn send_text(
    client: &reqwest::Client,
    base: &str,
    chat_id: i64,
    text: &str,
) -> Result<(), String> {
    for chunk in chunk_text(text, MAX_MESSAGE_LEN) {
        let resp = client
            .post(format!("{base}/sendMessage"))
            .timeout(Duration::from_secs(30))
            .json(&SendMessage {
                chat_id,
                text: &chunk,
            })
            .send()
            .await
            .map_err(|e| format!("发送消息失败: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("发送消息被拒绝: {}", resp.status()));
        }
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

/// 用户发来消息时记录"对方刚说话"（供主动引擎做频率控制）。
fn touch_user_reply(assistant: &dyn Host, external_id: &str) {
    let db = assistant.db();
    if let Ok(session) = crate::db::channel::get_or_create_session(&db, CHANNEL, external_id) {
        let _ = crate::db::proactive::touch_user_reply(&db, CHANNEL, external_id, &session.id);
    }
}

/// 命令类回执：没有后续记忆沉淀需求（`user_message_id` 为空即代表"无需沉淀"）。
fn plain_reply(reply: &str) -> ChatOutput {
    ChatOutput {
        session_id: String::new(),
        reply: reply.to_string(),
        user_message_id: String::new(),
        memory: None,
    }
}

/// 处理一条 TG 文本消息，返回本轮对话结果（None 表示无需回复）。
///
/// 只负责产出回复；记忆沉淀由调用方在回复送达之后另起后台任务（见 `run_loop`）。
async fn handle_text(
    assistant: Arc<dyn Host>,
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
            let db = assistant.db();
            crate::db::channel::reset_session(&db, CHANNEL, &external)
                .map_err(|e| e.to_string())?;
            Ok(Some(plain_reply("已开始新对话，之前的记录保留在记忆库里。")))
        }
        _ => {
            let session_id = {
                let db = assistant.db();
                crate::db::channel::get_or_create_session(&db, CHANNEL, &external)
                    .map(|s| s.id)
                    .map_err(|e| e.to_string())?
            };
            // 同步的对话流水线要在阻塞线程池里跑（api/chat.rs 同理）。
            let mut out = tokio::task::spawn_blocking(move || {
                let input = ChatInput {
                    message: text.to_string(),
                    session_id: Some(session_id),
                    history: Vec::new(),
                    history_count: None,
                    user_time,
                };
                assistant
                    .chat_stream(&input, &mut |_| {})
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("对话任务失败: {e}"))??;
            if out.reply.trim().is_empty() {
                out.reply = "(空回复)".to_string();
            }
            Ok(Some(out))
        }
    }
}

/// 长轮询入口。token 为空时直接返回，由 server 入口决定是否调用。
/// 异常退出后自动重连（启动期网络抖动不再导致永久掉线）。
pub async fn run(assistant: Arc<Assistant>) {
    let token = Config::get().telegram_bot_token.trim().to_string();
    if token.is_empty() {
        return;
    }
    loop {
        match run_loop(assistant.clone(), &token).await {
            Ok(()) => break, // run_loop 正常情况下永不返回
            Err(e) => {
                tracing::error!(error = %e, "telegram 轮询异常退出，60s 后重连");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }
}

async fn run_loop(assistant: Arc<Assistant>, token: &str) -> Result<(), String> {
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

    // 丢弃启动前积压的 update，避免重启后重复回复
    //（offset 语义：小于 offset 的 update 视为已确认）。
    let mut offset: i64 = {
        let updates: ApiResponse<Vec<Update>> = api_get(
            &client,
            &format!("{base}/getUpdates?timeout=0&limit=1&offset=-1"),
        )
        .await?;
        updates
            .result
            .unwrap_or_default()
            .into_iter()
            .map(|u| u.update_id + 1)
            .max()
            .unwrap_or(0)
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

        for update in batch {
            offset = offset.max(update.update_id + 1);
            let Some(message) = update.message else { continue };
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
            tracing::info!(chat_id, "收到消息，开始处理（进入 AI 流水线前）");

            touch_user_reply(assistant.as_ref(), &chat_id.to_string());

            send_typing(&client, &base, chat_id).await;
            let handle_started = std::time::Instant::now();

            let (next_reply, is_fallback, pending_memory) = match tokio::time::timeout(
                Duration::from_secs(super::AI_DEADLINE_SECS),
                handle_text(assistant.clone(), chat_id, &text, user_time),
            )
            .await
            {
                Ok(Ok(Some(out))) => {
                    tracing::info!(
                        chat_id,
                        elapsed_ms = handle_started.elapsed().as_millis() as u64,
                        "handle_text 返回（回复生产阶段完成）"
                    );
                    // 有 user_message_id 才需要沉淀（命令类回执没有）。
                    let pending = (!out.user_message_id.is_empty())
                        .then(|| (out.session_id.clone(), out.user_message_id.clone()));
                    (out.reply, false, pending)
                }
                Ok(Ok(None)) => continue,
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "对话失败");
                    ("刚才走神了，再发一次试试。".to_string(), true, None)
                }
                Err(_) => {
                    tracing::warn!(
                        chat_id,
                        deadline_secs = super::AI_DEADLINE_SECS,
                        "单轮对话超时，放弃本轮，继续轮询"
                    );
                    ("刚才走神了，再发一次试试。".to_string(), true, None)
                }
            };

            if let Err(e) = send_text(&client, &base, chat_id, &next_reply).await {
                tracing::error!(error = %e, chat_id, "回复发送失败");
            } else {
                tracing::info!(chat_id, is_fallback, "回复已发送，进入后置处理");
                // 回复已送达，再后台补记忆沉淀：判定/事实化要调 LLM，不能占用户等待时间。
                if let Some((session_id, user_message_id)) = pending_memory {
                    let assistant = assistant.clone();
                    let text = text.clone();
                    tokio::task::spawn_blocking(move || {
                        tracing::debug!(session_id = %session_id, "记忆沉淀(settle_memory)开始");
                        let _ = assistant.settle_memory(&session_id, &user_message_id, &text);
                        tracing::debug!(session_id = %session_id, "记忆沉淀(settle_memory)结束");
                    });
                }
                if !is_fallback {
                    // 正常回复发送成功：按语境决定是否像真人一样接着补几句。
                    // 延续判断同样要调 LLM，套同一个看门狗，避免拖死轮询。
                    let cont_started = std::time::Instant::now();
                    let _ = tokio::time::timeout(
                        Duration::from_secs(super::AI_DEADLINE_SECS),
                        crate::proactive::continuation::after_reply(
                            assistant.clone(),
                            CHANNEL,
                            &chat_id.to_string(),
                        ),
                    )
                    .await;
                    tracing::info!(
                        chat_id,
                        elapsed_ms = cont_started.elapsed().as_millis() as u64,
                        "延续判断(after_reply)结束"
                    );
                }
            }
        }
    }
}

