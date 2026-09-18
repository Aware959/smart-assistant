//! 微信 iLink 通道（腾讯官方 ClawBot 协议，`ilinkai.weixin.qq.com`）。
//!
//! 流程：扫码登录拿 `bot_token`（持久化到会话文件）→ `getupdates` 长轮询
//! 收消息 → 调内部对话流水线 → `sendmessage` 回复（必须回传 `context_token`）。
//!
//! 组成：
//! - [`protocol`]：报文类型 + HTTP 封装 + 扫码登录（不涉业务）；
//! - [`session`]：扫码凭证的持久化；
//! - [`push`]：窗口式主动推送（登录会话 + 每用户 context_token 缓存）；
//! - 本模块：轮询主循环、消息处理与通道入口。
//!
//! 与 Telegram 的差异：
//! - 无 AppSecret，只能扫码登录；token 失效（会话过期）后需重新扫码；
//! - **主动推送是"窗口式"的**：需要用户先发消息拿到 `context_token`，服务端缓存后
//!   可在窗口内（约 24h / 10 次外发的保守取值）主动发送；超窗或超次数须等用户
//!   再次入站刷新。对外表现由 [`super::IlinkRegistry`] 管理；
//! - 每个请求头需携带随机 `X-WECHAT-UIN`。

mod protocol;
mod push;
mod session;

pub(crate) use push::{push_send, set_session};
pub(crate) use session::SavedSession;

use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::host::Host;
use crate::{Assistant, ChatInput, ChatOutput};

const CHANNEL: &str = "ilink";
pub(crate) const DEFAULT_BASE: &str = "https://ilinkai.weixin.qq.com";
pub(crate) const CHANNEL_VERSION: &str = "1.0.2";
/// iLink 单条文本保守上限（官方未公开，按微信习惯取 2000 字符分片）。
pub(crate) const MAX_MESSAGE_LEN: usize = 2000;
/// getupdates 客户端超时（服务端 hold 约 35s，留余量）。
const POLL_TIMEOUT_SECS: u64 = 60;
/// 轮询异常后的退避等待（秒）。
const RETRY_DELAY_SECS: u64 = 5;
/// 外层重连等待（秒）。
const RECONNECT_DELAY_SECS: u64 = 60;
/// 扫码状态轮询间隔（秒）。
pub(crate) const QR_POLL_INTERVAL_SECS: u64 = 2;

/// 公共请求体里的 `base_info` 对象。
pub(crate) fn base_info() -> serde_json::Value {
    serde_json::json!({ "channel_version": CHANNEL_VERSION })
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

/// 处理一条用户文本消息，返回本轮对话结果。
///
/// 只负责产出回复；记忆沉淀由调用方在回复送达之后另起后台任务（见 `run_loop`）。
async fn handle_text(
    assistant: Arc<Assistant>,
    from_user_id: String,
    text: String,
    user_time: Option<String>,
) -> Result<ChatOutput, String> {
    match text.trim() {
        "/new" => {
            let db = assistant.db();
            crate::db::channel::reset_session(&db, CHANNEL, &from_user_id)
                .map_err(|e| e.to_string())?;
            Ok(plain_reply("已开始新对话，之前的记录保留在记忆库里。"))
        }
        _ => {
            let session_id = {
                let db = assistant.db();
                crate::db::channel::get_or_create_session(&db, CHANNEL, &from_user_id)
                    .map(|s| s.id)
                    .map_err(|e| e.to_string())?
            };
            // 同步的对话流水线要在阻塞线程池里跑（api/chat.rs 同理）。
            let mut out = tokio::task::spawn_blocking(move || {
                let input = ChatInput {
                    message: text,
                    session_id: Some(session_id),
                    history: Vec::new(),
                    history_count: None,
                    user_time,
                };
                assistant
                    .chat_stream(&input, |_| {})
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("对话任务失败: {e}"))??;
            if out.reply.trim().is_empty() {
                out.reply = "(空回复)".to_string();
            }
            Ok(out)
        }
    }
}

/// 轮询正常时永不返回；只在会话过期时返回 Relogin。
enum PollResult {
    Relogin,
}

async fn run_loop(
    assistant: Arc<Assistant>,
    client: &reqwest::Client,
    sess: &SavedSession,
) -> Result<PollResult, String> {
    let base = if sess.baseurl.trim().is_empty() {
        DEFAULT_BASE.to_string()
    } else {
        sess.baseurl.clone()
    };
    let allowed = Config::get().ilink_allowed_ids.clone();
    let mut cursor = String::new();

    loop {
        let updates: protocol::UpdatesResponse = match protocol::api_post(
            client,
            &format!("{base}/ilink/bot/getupdates"),
            &sess.bot_token,
            &serde_json::json!({
                "get_updates_buf": cursor,
                "base_info": base_info(),
            }),
            POLL_TIMEOUT_SECS + 20,
        )
        .await
        {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, "getupdates 失败，{RETRY_DELAY_SECS}s 后重试");
                tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)).await;
                continue;
            }
        };

        if protocol::is_session_expired(&updates.err) {
            let detail = updates
                .err
                .as_ref()
                .and_then(|e| e.message.clone())
                .unwrap_or_default();
            tracing::warn!(detail = %detail, "iLink 会话过期，需重新扫码登录");
            return Ok(PollResult::Relogin);
        }
        if let Some(buf) = updates.get_updates_buf {
            if !buf.is_empty() {
                cursor = buf;
            }
        }

        for msg in &updates.msgs {
            let Some(text) = protocol::extract_text(msg) else {
                continue;
            };
            if !allowed.is_empty() && !allowed.contains(&msg.from_user_id) {
                tracing::info!(from = %msg.from_user_id, "未在白名单，忽略");
                continue;
            }
            if !msg.group_id.as_deref().unwrap_or_default().is_empty() {
                tracing::info!(group = ?msg.group_id, "暂只处理单聊，跳过群消息");
                continue;
            }

            push::refresh_contact(&msg.from_user_id, &msg.context_token);
            touch_user_reply(assistant.as_ref(), &msg.from_user_id);

            let user_time = super::ts_to_rfc3339(msg.create_time_ms);
            if msg.create_time_ms.is_none() {
                let keys: Vec<&String> = msg.extra.keys().collect();
                tracing::debug!(keys = ?keys, "iLink 报文未命中时间戳字段，实际键名：");
            } else {
                tracing::debug!(create_time_ms = ?msg.create_time_ms, user_time = ?user_time, "入站消息时间戳（用于时间世界模型）");
            }

            protocol::send_typing(client, &base, sess, &msg.context_token).await;

            // 后置沉淀要用原文做兜底，而 handle_text 会接管 text，先留一份。
            let settle_text = text.clone();
            let (reply, is_fallback, pending_memory) = match tokio::time::timeout(
                Duration::from_secs(super::AI_DEADLINE_SECS),
                handle_text(
                    assistant.clone(),
                    msg.from_user_id.clone(),
                    text,
                    user_time,
                ),
            )
            .await
            {
                Ok(Ok(out)) => {
                    // 有 user_message_id 才需要沉淀（命令类回执没有）。
                    let pending = (!out.user_message_id.is_empty()).then(|| {
                        (
                            out.session_id.clone(),
                            out.user_message_id.clone(),
                            settle_text,
                        )
                    });
                    (out.reply, false, pending)
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "对话失败");
                    ("刚才走神了，再发一次试试。".to_string(), true, None)
                }
                Err(_) => {
                    tracing::warn!(
                        from = %msg.from_user_id,
                        deadline_secs = super::AI_DEADLINE_SECS,
                        "单轮对话超时，放弃本轮，继续轮询"
                    );
                    ("刚才走神了，再发一次试试。".to_string(), true, None)
                }
            };

            if let Err(e) = protocol::send_text(
                client,
                &base,
                sess,
                &msg.from_user_id,
                &msg.context_token,
                &reply,
            )
            .await
            {
                tracing::error!(error = %e, "回复发送失败");
            } else {
                // 回复已送达，再后台补记忆沉淀：判定/事实化要调 LLM，不能占用户等待时间。
                if let Some((session_id, user_message_id, text)) = pending_memory {
                    let assistant = assistant.clone();
                    tokio::task::spawn_blocking(move || {
                        let _ = assistant.settle_memory(&session_id, &user_message_id, &text);
                    });
                }
                if !is_fallback {
                    // 正常回复发送成功：按语境决定是否像机器人一样接着补几句。
                    // 延续判断同样要调 LLM，套同一个看门狗，避免拖死轮询。
                    let _ = tokio::time::timeout(
                        Duration::from_secs(super::AI_DEADLINE_SECS),
                        crate::proactive::continuation::after_reply(
                            assistant.clone(),
                            CHANNEL,
                            &msg.from_user_id,
                        ),
                    )
                    .await;
                }
            }
        }
    }
}

/// iLink 通道入口：仅在 `ILINK_ENABLED=1` 时启动（有 token 直连，无 token 扫码登录）。
/// 需外显关闭时设 `ILINK_ENABLED=0`，即使已有会话文件也不会再连接。
pub async fn run(assistant: Arc<Assistant>) {
    let cfg = Config::get();
    if !cfg.ilink_enabled {
        tracing::info!("iLink 通道未启用（设置 ILINK_ENABLED=1 开启）");
        return;
    }
    let session_file = cfg.ilink_session_file.clone();

    // token 来源：环境变量 > 会话文件。
    let mut sess = session::load(&session_file);
    if !cfg.ilink_bot_token.trim().is_empty() {
        sess.bot_token = cfg.ilink_bot_token.trim().to_string();
    }
    if sess.bot_token.trim().is_empty() {
        tracing::info!("iLink 无有效 token，将进入扫码登录");
    }

    let client = match reqwest::Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "创建 HTTP 客户端失败，iLink 通道未启动");
            return;
        }
    };

    loop {
        // 无有效 token → 走扫码登录。
        if sess.bot_token.trim().is_empty() {
            match protocol::login(&client, DEFAULT_BASE).await {
                Ok(s) => {
                    if let Err(e) = session::save(&session_file, &s) {
                        tracing::error!(error = %e, "保存会话失败，下次重启需重新扫码");
                    }
                    set_session(s.clone(), true);
                    sess = s;
                }
                Err(e) => {
                    tracing::error!(error = %e, "扫码登录失败，{RECONNECT_DELAY_SECS}s 后重试");
                    tokio::time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
                    continue;
                }
            }
        } else {
            set_session(sess.clone(), true);
        }

        tracing::info!("iLink 通道已连接，开始长轮询");
        match run_loop(assistant.clone(), &client, &sess).await {
            Ok(PollResult::Relogin) => {
                // 会话过期：清掉旧凭证，下一轮重新扫码。
                sess = SavedSession::default();
                let _ = std::fs::remove_file(&session_file);
                continue;
            }
            Err(e) => {
                tracing::error!(error = %e, "iLink 轮询异常退出，{RECONNECT_DELAY_SECS}s 后重连");
                tokio::time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
            }
        }
    }
}