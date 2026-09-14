//! 微信 iLink 通道（腾讯官方 ClawBot 协议，`ilinkai.weixin.qq.com`）。
//!
//! 流程：扫码登录拿 `bot_token`（持久化到会话文件）→ `getupdates` 长轮询
//! 收消息 → 调内部对话流水线 → `sendmessage` 回复（必须回传 `context_token`）。
//!
//! 与 Telegram 的差异：
//! - 无 AppSecret，只能扫码登录；token 失效（会话过期）后需重新扫码；
//! - **主动推送是"窗口式"的**：需要用户先发消息拿到 `context_token`，服务端缓存后
//!   可在窗口内（约 24h / 10 次外发的保守取值）主动发送；超窗或超次数须等用户
//!   再次入站刷新。对外表现由 [`super::IlinkRegistry`] 管理；
//! - 每个请求头需携带随机 `X-WECHAT-UIN`。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::{Assistant, ChatInput};

use super::chunk_text;

const CHANNEL: &str = "ilink";
const DEFAULT_BASE: &str = "https://ilinkai.weixin.qq.com";
const CHANNEL_VERSION: &str = "1.0.2";
/// iLink 单条文本保守上限（官方未公开，按微信习惯取 2000 字符分片）。
const MAX_MESSAGE_LEN: usize = 2000;
/// getupdates 客户端超时（服务端 hold 约 35s，留余量）。
const POLL_TIMEOUT_SECS: u64 = 60;
/// 轮询异常后的退避等待（秒）。
const RETRY_DELAY_SECS: u64 = 5;
/// 外层重连等待（秒）。
const RECONNECT_DELAY_SECS: u64 = 60;
/// 扫码状态轮询间隔（秒）。
const QR_POLL_INTERVAL_SECS: u64 = 2;

// ---------- 会话文件 ----------

/// 持久化扫码登录凭证，避免每次重启都重新扫码。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SavedSession {
    #[serde(default)]
    bot_token: String,
    #[serde(default)]
    baseurl: String,
    #[serde(default)]
    ilink_bot_id: String,
    #[serde(default)]
    ilink_user_id: String,
}

fn load_session(path: &str) -> SavedSession {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_session(path: &str, sess: &SavedSession) -> Result<(), String> {
    let data =
        serde_json::to_string_pretty(sess).map_err(|e| format!("序列化会话失败: {e}"))?;
    std::fs::write(path, data).map_err(|e| format!("写入会话文件失败: {e}"))?;
    Ok(())
}

// ---------- 协议类型 ----------

/// 宽松取值：兼容顶层字段与 `data` 包裹两种形状，多个候选键按序尝试。
fn get_str(v: &serde_json::Value, keys: &[&str]) -> String {
    for key in keys {
        for candidate in [
            v.get(key),
            v.get("data").and_then(|d| d.get(key)),
        ] {
            if let Some(s) = candidate.and_then(|x| x.as_str()) {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    String::new()
}

/// 脱敏：递归把键名含 token 的值替换为 ***，用于日志。
fn redact(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, val)| {
                    if k.to_lowercase().contains("token") {
                        (k.clone(), serde_json::Value::String("***".to_string()))
                    } else {
                        (k.clone(), redact(val))
                    }
                })
                .collect(),
        ),
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(redact).collect())
        }
        _ => v.clone(),
    }
}

#[derive(Debug, Deserialize)]
struct ApiErr {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: Option<String>,
}

/// 会话过期（文档与社区实现均指向 code -14）：需重新扫码登录。
fn is_session_expired(err: &Option<ApiErr>) -> bool {
    matches!(err, Some(e) if e.code == -14)
}

#[derive(Debug, Deserialize)]
struct UpdatesResponse {
    #[serde(default)]
    msgs: Vec<RawMsg>,
    #[serde(default)]
    get_updates_buf: Option<String>,
    #[serde(default)]
    err: Option<ApiErr>,
}

#[derive(Debug, Deserialize)]
struct RawMsg {
    #[serde(default)]
    message_type: i64,
    #[serde(default)]
    from_user_id: String,
    #[serde(default)]
    context_token: String,
    #[serde(default)]
    group_id: Option<String>,
    #[serde(default)]
    item_list: Vec<RawItem>,
    /// 消息发出时间戳（已实测确认的 iLink 报文字段：`create_time_ms`，Unix 毫秒）。
    #[serde(default)]
    create_time_ms: Option<i64>,
    /// 其余未解析字段（探测真实时间戳字段名用，日志只记录键名不记录内容）。
    #[serde(default, flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawItem {
    #[serde(rename = "type", default)]
    kind: i64,
    #[serde(default)]
    text_item: Option<TextItem>,
}

#[derive(Debug, Deserialize)]
struct TextItem {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct ConfigResponse {
    #[serde(default)]
    typing_ticket: Option<String>,
}

/// 只处理用户文本消息（message_type 1），拼接全部文本片段。
fn extract_text(msg: &RawMsg) -> Option<String> {
    if msg.message_type != 1 {
        return None;
    }
    let text: String = msg
        .item_list
        .iter()
        .filter(|i| i.kind == 1)
        .filter_map(|i| i.text_item.as_ref())
        .map(|t| t.text.as_str())
        .collect();
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

// ---------- 请求 ----------

/// 随机 uint32 → 十进制字符串 → base64（协议要求的 X-WECHAT-UIN）。
fn random_uin() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    base64::engine::general_purpose::STANDARD.encode(n.to_string())
}

fn business_headers(token: &str) -> Result<reqwest::header::HeaderMap, String> {
    use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
    let mut headers = HeaderMap::new();
    headers.insert(
        "AuthorizationType",
        HeaderValue::from_static("ilink_bot_token"),
    );
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("无效的 token: {e}"))?,
    );
    headers.insert(
        "X-WECHAT-UIN",
        HeaderValue::from_str(&random_uin()).map_err(|e| format!("构造请求头失败: {e}"))?,
    );
    Ok(headers)
}

fn base_info() -> serde_json::Value {
    serde_json::json!({ "channel_version": CHANNEL_VERSION })
}

async fn api_post<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &serde_json::Value,
    timeout_secs: u64,
) -> Result<T, String> {
    let resp = client
        .post(url)
        .headers(business_headers(token)?)
        .timeout(Duration::from_secs(timeout_secs))
        .json(body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("服务端拒绝: {}", resp.status()));
    }
    resp.json::<T>()
        .await
        .map_err(|e| format!("解析响应失败: {e}"))
}

// ---------- 扫码登录 ----------

fn render_qr(content: &str) -> String {
    qrcode::QrCode::new(content.as_bytes())
        .map(|qr| {
            qr.render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .build()
        })
        .unwrap_or_else(|_| content.to_string())
}

async fn get_json(
    client: &reqwest::Client,
    url: &str,
    extra_version_header: bool,
) -> Result<serde_json::Value, String> {
    let mut req = client.get(url).timeout(Duration::from_secs(15));
    if extra_version_header {
        req = req.header("iLink-App-ClientVersion", "1");
    }
    let text = req
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("响应不是 JSON: {e}；原文: {text:?}"))
}

/// 扫码登录：取二维码 → 终端展示 → 轮询确认。二维码过期自动重取。
///
/// 响应按 `serde_json::Value` 宽松解析（兼容顶层 / `data` 包裹、多种键名），
/// 遇到非常规状态会把脱敏后的原文打到日志，便于对照协议文档排查。
async fn login(client: &reqwest::Client, base: &str) -> Result<SavedSession, String> {
    loop {
        let qr = get_json(
            client,
            &format!("{base}/ilink/bot/get_bot_qrcode?bot_type=3"),
            false,
        )
        .await
        .map_err(|e| format!("获取二维码失败: {e}"))?;
        let qrcode = get_str(&qr, &["qrcode", "qr_code", "qrcode_key"]);
        if qrcode.is_empty() {
            return Err(format!("服务端未返回二维码，原文: {:?}", redact(&qr)));
        }
        // qrcode_img_content 是微信可识别的二维码内容，优先用它渲染。
        let payload = get_str(&qr, &["qrcode_img_content", "qrcode_url", "qr_img"]);
        let payload = if payload.is_empty() { qrcode.clone() } else { payload };
        tracing::info!(
            "\n微信扫码登录（bot_type=3）：\n{}\n扫码后在手机上确认，超时会自动刷新二维码。",
            render_qr(&payload)
        );

        let mut last_status = String::new();
        loop {
            tokio::time::sleep(Duration::from_secs(QR_POLL_INTERVAL_SECS)).await;
            let st = get_json(
                client,
                &format!("{base}/ilink/bot/get_qrcode_status?qrcode={qrcode}"),
                true,
            )
            .await
            .map_err(|e| format!("查询扫码状态失败: {e}"))?;
            let status = get_str(&st, &["status", "state", "qrcode_status"]);
            let token = get_str(&st, &["bot_token", "token", "access_token"]);
            match status.as_str() {
                "confirmed" if !token.is_empty() => {
                    let baseurl = get_str(&st, &["baseurl", "base_url"]);
                    tracing::info!("微信扫码确认成功");
                    return Ok(SavedSession {
                        bot_token: token,
                        baseurl: if baseurl.is_empty() {
                            base.to_string()
                        } else {
                            baseurl
                        },
                        ilink_bot_id: get_str(&st, &["ilink_bot_id", "bot_id"]),
                        ilink_user_id: get_str(&st, &["ilink_user_id", "user_id"]),
                    });
                }
                // 已确认但 token 还没下发：继续等。
                "confirmed" => continue,
                "expired" => {
                    tracing::info!("二维码过期，重新获取");
                    break;
                }
                // wait / scaned：正常等待；其它非常规状态只在变化时打一次日志。
                _ => {
                    if status != "wait" && status != "scaned" && status != last_status {
                        last_status = status.clone();
                        tracing::info!(
                            "非常规扫码状态 {status:?}，原文: {:?}",
                            redact(&st)
                        );
                    }
                    continue;
                }
            }
        }
    }
}

// ---------- 消息收发 ----------

async fn send_typing(
    client: &reqwest::Client,
    base: &str,
    sess: &SavedSession,
    context_token: &str,
) {
    if sess.ilink_user_id.trim().is_empty() {
        return;
    }
    // 先取 ticket，再发 typing；失败则静默跳过（尽力而为）。
    let config: Result<ConfigResponse, String> = api_post(
        client,
        &format!("{base}/ilink/bot/getconfig"),
        &sess.bot_token,
        &serde_json::json!({
            "ilink_user_id": sess.ilink_user_id,
            "context_token": context_token,
            "base_info": base_info(),
        }),
        10,
    )
    .await;
    let ticket = config.ok().and_then(|c| c.typing_ticket).unwrap_or_default();
    if ticket.trim().is_empty() {
        return;
    }
    let _: Result<serde_json::Value, String> = api_post(
        client,
        &format!("{base}/ilink/bot/sendtyping"),
        &sess.bot_token,
        &serde_json::json!({
            "ilink_user_id": sess.ilink_user_id,
            "typing_ticket": ticket,
            "status": 1,
            "base_info": base_info(),
        }),
        10,
    )
    .await;
}

async fn send_text(
    client: &reqwest::Client,
    base: &str,
    sess: &SavedSession,
    to_user_id: &str,
    context_token: &str,
    text: &str,
) -> Result<(), String> {
    for chunk in chunk_text(text, MAX_MESSAGE_LEN) {
        let _: serde_json::Value = api_post(
            client,
            &format!("{base}/ilink/bot/sendmessage"),
            &sess.bot_token,
            &serde_json::json!({
                "msg": {
                    "from_user_id": "",
                    "to_user_id": to_user_id,
                    "client_id": format!("smart-assistant-{}", uuid::Uuid::new_v4()),
                    "message_type": 2,
                    "message_state": 2,
                    "context_token": context_token,
                    "item_list": [{ "type": 1, "text_item": { "text": chunk } }],
                },
                "base_info": base_info(),
            }),
            20,
        )
        .await?;
    }
    Ok(())
}

// ---------- 主动推送（窗口式） ----------

fn push_registry() -> &'static std::sync::Mutex<Option<super::IlinkRegistry>> {
    &super::shared_push().ilink
}

/// 登录会话更新：登录/重登录后调用（重登录时清空全部用户 token 缓存）。
pub(crate) fn set_session(sess: SavedSession, drop_contacts: bool) {
    let mut reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
    let entry = reg.get_or_insert_with(super::IlinkRegistry::default);
    entry.sess = Some(sess);
    if drop_contacts {
        entry.contacts.clear();
    }
}

/// 入站消息刷新某用户的 context_token（同时作为"对方刚说话"的证明）。
fn refresh_contact(from_user_id: &str, context_token: &str) {
    if context_token.trim().is_empty() {
        return;
    }
    let mut reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(entry) = reg.as_mut() {
        entry.contacts.insert(
            from_user_id.to_string(),
            super::IlinkContact {
                context_token: context_token.to_string(),
                observed_at: chrono::Utc::now(),
                sends_since_refresh: 0,
            },
        );
    }
}

/// 主动推送：仅当该用户带新鲜 context_token 时才会真正发送。
/// 返回 Err 表示当前无可用窗口（token 缺失/过期/超次），调用方应跳过该用户。
pub(crate) async fn push_send(external_id: &str, text: &str) -> Result<(), String> {
    let cfg = Config::get();
    let (sess, token) = {
        let reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = reg.as_ref() else {
            return Err("iLink 尚未登录".to_string());
        };
        let Some(sess) = entry.sess.clone() else {
            return Err("iLink 会话为空".to_string());
        };
        let Some(contact) = entry.contacts.get(external_id) else {
            return Err("该用户无 context_token（需先主动发消息）".to_string());
        };
        if !contact.is_fresh(chrono::Utc::now(), cfg.ilink_context_max_age_hours) {
            return Err("context_token 已过窗口，等待用户再次入站刷新".to_string());
        }
        (sess, contact.context_token.clone())
    };

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let base = if sess.baseurl.trim().is_empty() {
        DEFAULT_BASE.to_string()
    } else {
        sess.baseurl.clone()
    };

    match send_text(&client, &base, &sess, external_id, &token, text).await {
        Ok(()) => {
            // 发送成功：递增窗口计数（iLink 200 不代表 100% 投递，按协议层成功计，
            // 用户下次入站自然会刷新窗口）。
            let mut reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = reg.as_mut() {
                if let Some(c) = entry.contacts.get_mut(external_id) {
                    c.sends_since_refresh += 1;
                }
            }
            Ok(())
        }
        Err(e) => {
            tracing::warn!(
                user = external_id,
                error = %e,
                "iLink 主动推送失败，丢弃该用户 token"
            );
            let mut reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = reg.as_mut() {
                entry.contacts.remove(external_id);
            }
            Err(e)
        }
    }
}

/// 用户发来消息时记录"对方刚说话"（供主动引擎做频率控制）。
fn touch_user_reply(assistant: &Assistant, external_id: &str) {
    let db = assistant.inner_db();
    if let Ok(session) = crate::db::channel::get_or_create_session(&db, CHANNEL, external_id) {
        let _ = crate::db::proactive::touch_user_reply(&db, CHANNEL, external_id, &session.id);
    }
}

/// 处理一条用户文本消息，返回回复文本。
async fn handle_text(
    assistant: Arc<Assistant>,
    from_user_id: String,
    text: String,
    user_time: Option<String>,
) -> Result<String, String> {
    match text.trim() {
        "/new" => {
            let db = assistant.inner_db();
            crate::db::channel::reset_session(&db, CHANNEL, &from_user_id)
                .map_err(|e| e.to_string())?;
            Ok("已开始新对话，之前的记录保留在记忆库里。".to_string())
        }
        _ => {
            let session_id = {
                let db = assistant.inner_db();
                crate::db::channel::get_or_create_session(&db, CHANNEL, &from_user_id)
                    .map(|s| s.id)
                    .map_err(|e| e.to_string())?
            };
            // 同步的对话流水线要在阻塞线程池里跑（api/chat.rs 同理）。
            let reply = tokio::task::spawn_blocking(move || {
                let input = ChatInput {
                    message: text,
                    session_id: Some(session_id),
                    history: Vec::new(),
                    history_count: None,
                    user_time,
                };
                assistant
                    .chat_stream(&input, |_| {})
                    .map(|out| out.reply)
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("对话任务失败: {e}"))??;
            Ok(if reply.trim().is_empty() {
                "(空回复)".to_string()
            } else {
                reply
            })
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
        let updates: UpdatesResponse = match api_post(
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

        if is_session_expired(&updates.err) {
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
            let Some(text) = extract_text(msg) else {
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

            refresh_contact(&msg.from_user_id, &msg.context_token);
            touch_user_reply(&assistant, &msg.from_user_id);

            let user_time = super::ts_to_rfc3339(msg.create_time_ms);
            if msg.create_time_ms.is_none() {
                let keys: Vec<&String> = msg.extra.keys().collect();
                tracing::debug!(keys = ?keys, "iLink 报文未命中时间戳字段，实际键名：");
            } else {
                tracing::debug!(create_time_ms = ?msg.create_time_ms, user_time = ?user_time, "入站消息时间戳（用于时间世界模型）");
            }

            send_typing(client, &base, sess, &msg.context_token).await;

            let (reply, is_fallback) = match handle_text(
                assistant.clone(),
                msg.from_user_id.clone(),
                text,
                user_time,
            )
            .await
            {
                Ok(reply) => (reply, false),
                Err(e) => {
                    tracing::error!(error = %e, "对话失败");
                    ("刚才走神了，再发一次试试。".to_string(), true)
                }
            };

            if let Err(e) = send_text(
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
            } else if !is_fallback {
                // 正常回复发送成功：按语境决定是否像真人一样接着补几句。
                crate::proactive::continuation::after_reply(
                    assistant.clone(),
                    CHANNEL,
                    &msg.from_user_id,
                )
                .await;
            }
        }
    }
}

/// iLink 通道入口：无 token 且未启用时静默跳过；异常退出后自动重连。
pub async fn run(assistant: Arc<Assistant>) {
    let cfg = Config::get();
    let session_file = cfg.ilink_session_file.clone();

    // token 来源：环境变量 > 会话文件。
    let mut sess = load_session(&session_file);
    if !cfg.ilink_bot_token.trim().is_empty() {
        sess.bot_token = cfg.ilink_bot_token.trim().to_string();
    }
    if sess.bot_token.trim().is_empty() && !cfg.ilink_enabled {
        return;
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
            match login(&client, DEFAULT_BASE).await {
                Ok(s) => {
                    if let Err(e) = save_session(&session_file, &s) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_updates_and_extracts_text() {
        let raw = r#"{
            "msgs": [
                {
                    "message_type": 1,
                    "from_user_id": "o9cq800kum@im.wechat",
                    "to_user_id": "e06c1cee@im.bot",
                    "message_state": 2,
                    "context_token": "AARzJWAFAAABAAAAAAAp2m3u7oE0x7V8Xw==",
                    "item_list": [{ "type": 1, "text_item": { "text": "你好" } }]
                },
                {
                    "message_type": 2,
                    "from_user_id": "e06c1cee@im.bot",
                    "context_token": "AAA=",
                    "item_list": [{ "type": 1, "text_item": { "text": "bot 回声" } }]
                },
                {
                    "message_type": 1,
                    "from_user_id": "img@im.wechat",
                    "context_token": "BBB=",
                    "item_list": [{ "type": 2 }]
                }
            ],
            "get_updates_buf": "eyJzZXEiOjQyOH0="
        }"#;
        let updates: UpdatesResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(updates.msgs.len(), 3);
        assert_eq!(
            updates.get_updates_buf.as_deref(),
            Some("eyJzZXEiOjQyOH0=")
        );

        // 用户文本消息可提取
        assert_eq!(
            extract_text(&updates.msgs[0]).as_deref(),
            Some("你好")
        );
        // bot 自己的回声跳过
        assert!(extract_text(&updates.msgs[1]).is_none());
        // 非文本 item 跳过
        assert!(extract_text(&updates.msgs[2]).is_none());
    }

    #[test]
    fn parses_qr_status_confirmed_flat_and_nested() {
        let flat: serde_json::Value = serde_json::from_str(
            r#"{
            "status": "confirmed",
            "bot_token": "ilinkbot_abc123",
            "ilink_bot_id": "e06c1cee@im.bot",
            "ilink_user_id": "o9cq800kum@im.wechat",
            "baseurl": "https://ilinkai.weixin.qq.com"
        }"#,
        )
        .unwrap();
        assert_eq!(get_str(&flat, &["status", "state"]), "confirmed");
        assert_eq!(
            get_str(&flat, &["bot_token", "token", "access_token"]),
            "ilinkbot_abc123"
        );

        // data 包裹形状与候选键名同样能取到
        let nested: serde_json::Value = serde_json::from_str(
            r#"{"data": {"state": "confirmed", "access_token": "tok_xyz"}}"#,
        )
        .unwrap();
        assert_eq!(get_str(&nested, &["status", "state"]), "confirmed");
        assert_eq!(
            get_str(&nested, &["bot_token", "token", "access_token"]),
            "tok_xyz"
        );
    }

    #[test]
    fn detects_session_expired() {
        let err = Some(ApiErr {
            code: -14,
            message: Some("session expired".to_string()),
        });
        assert!(is_session_expired(&err));
        assert!(!is_session_expired(&None));
        assert!(!is_session_expired(&Some(ApiErr {
            code: 0,
            message: None
        })));
    }

    #[test]
    fn uin_is_base64_of_decimal_uint32() {
        let uin = random_uin();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&uin)
            .unwrap();
        let decimal = String::from_utf8(decoded).unwrap();
        assert!(decimal.parse::<u32>().is_ok(), "应为十进制 uint32");
    }

    #[test]
    fn business_headers_have_required_fields() {
        let headers = business_headers("test_token_123").unwrap();
        assert_eq!(
            headers.get("AuthorizationType").unwrap(),
            "ilink_bot_token"
        );
        assert_eq!(
            headers.get(reqwest::header::AUTHORIZATION).unwrap(),
            "Bearer test_token_123"
        );
        assert!(headers.get("X-WECHAT-UIN").is_some());
    }
}
