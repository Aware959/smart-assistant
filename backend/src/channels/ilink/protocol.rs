//! iLink 协议层：报文类型、宽松解析、HTTP 请求封装与扫码登录。
//!
//! 不含业务：不做会话映射、不调对话流水线、不维护推送状态。

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine as _;
use serde::Deserialize;

use super::session::SavedSession;
use super::super::chunk_text;
use super::base_info;
use super::{MAX_MESSAGE_LEN, QR_POLL_INTERVAL_SECS};

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
pub(crate) struct ApiErr {
    #[serde(default)]
    pub(crate) code: i64,
    #[serde(default)]
    pub(crate) message: Option<String>,
}

/// 会话过期（文档与社区实现均指向 code -14）：需重新扫码登录。
pub(crate) fn is_session_expired(err: &Option<ApiErr>) -> bool {
    matches!(err, Some(e) if e.code == -14)
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdatesResponse {
    #[serde(default)]
    pub(crate) msgs: Vec<RawMsg>,
    #[serde(default)]
    pub(crate) get_updates_buf: Option<String>,
    #[serde(default)]
    pub(crate) err: Option<ApiErr>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawMsg {
    #[serde(default)]
    pub(crate) message_type: i64,
    #[serde(default)]
    pub(crate) from_user_id: String,
    #[serde(default)]
    pub(crate) context_token: String,
    #[serde(default)]
    pub(crate) group_id: Option<String>,
    #[serde(default)]
    pub(crate) item_list: Vec<RawItem>,
    /// 消息发出时间戳（已实测确认的 iLink 报文字段：`create_time_ms`，Unix 毫秒）。
    #[serde(default)]
    pub(crate) create_time_ms: Option<i64>,
    /// 其余未解析字段（探测真实时间戳字段名用，日志只记录键名不记录内容）。
    #[serde(default, flatten)]
    pub(crate) extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawItem {
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
pub(crate) fn extract_text(msg: &RawMsg) -> Option<String> {
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

pub(crate) async fn api_post<T: for<'de> Deserialize<'de>>(
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
pub(crate) async fn login(
    client: &reqwest::Client,
    base: &str,
) -> Result<SavedSession, String> {
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

pub(crate) async fn send_typing(
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

pub(crate) async fn send_text(
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