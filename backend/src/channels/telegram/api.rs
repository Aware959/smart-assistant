//! Telegram api 子模块。

use super::*;

pub(super) fn api_base(token: &str) -> String {
    format!("https://api.telegram.org/bot{token}")
}

pub(super) fn build_client() -> Result<reqwest::Client, String> {
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

use super::super::{chunk_text_utf16, status_retryable, with_send_retry, SendErr, SEND_RETRY};

pub(super) async fn api_get<T: for<'de> Deserialize<'de>>(
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
pub(super) async fn send_text(
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
pub(super) async fn send_action(
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
pub(super) async fn send_dice(
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
pub(super) fn dice_value(result: &Option<serde_json::Value>) -> Result<u8, String> {
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
pub(super) async fn send_sticker(
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
pub(super) fn encode_set_name(name: &str) -> String {
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
