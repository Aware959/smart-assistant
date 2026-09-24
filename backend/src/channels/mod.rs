//! 外部渠道接入：把第三方平台的消息翻译为内部对话流水线调用。
//!
//! 每个渠道只做三件事：收消息 → 调主线聊天服务（[`crate::core::agent::Agent::chat_stream`]）→ 回消息。
//! 会话隔离由会话服务（[`crate::core::session::Sessions`]）的 `resolve` / `reset` 负责。
//!
//! 主动推送：各渠道的 run 循环把"当前可推状态"写入 [`shared_push`]，主动引擎
//! 经 [`ChannelsPusher`]（主动外发的具体实现）只读查询后派发文本。

// 未启用任何通道特性时（如只编 sa-web），本模块整体处于惰性状态：
// 保留占位实现以维持 core::intent 的依赖成立，但无需再为死代码报警。
#![cfg_attr(
    not(any(feature = "telegram", feature = "ilink")),
    allow(dead_code, unused_imports, unused_variables)
)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[cfg(feature = "telegram")]
pub mod telegram;

#[cfg(feature = "ilink")]
pub mod ilink;

/// iLink 每次入站刷新后，最多允许的主动外发条数（协议窗口约束的保守取值）。
#[cfg(feature = "ilink")]
pub(crate) const ILINK_MAX_SENDS_PER_REFRESH: usize = 10;

/// 单条入站消息的 AI 处理看门狗上限（秒）：超过即放弃本轮、回兜底话术并继续轮询。
///
/// 阻塞线程池里的 LLM/embedding 调用即便有客户端超时，串行等待侧仍可能被一次
/// 停滞拖住整条长轮询；这里的 deadline 保证任何情况下轮询都能在有限时间内恢复。
pub(crate) const AI_DEADLINE_SECS: u64 = 240;

/// 全局推送状态注册表：由各通道 relay 维护，intent 引擎只读。
#[derive(Default)]
pub struct PushChannels {
    /// Telegram 长轮询已连通（token 有效）时置位，之后即可随时主动推。
    pub(crate) telegram_ready: std::sync::atomic::AtomicBool,
    #[cfg(feature = "ilink")]
    pub(crate) ilink: Mutex<Option<IlinkRegistry>>,
}

/// iLink 推送上下文：登录会话 + 每个用户的最近 context_token 缓存。
#[cfg(feature = "ilink")]
#[derive(Default)]
pub(crate) struct IlinkRegistry {
    pub(crate) sess: Option<crate::channels::ilink::SavedSession>,
    pub(crate) contacts: HashMap<String, IlinkContact>,
}

#[cfg(feature = "ilink")]
#[derive(Clone)]
pub(crate) struct IlinkContact {
    pub(crate) context_token: String,
    /// 该 token 最后被（入站消息）刷新的时刻。
    pub(crate) observed_at: chrono::DateTime<chrono::Utc>,
    /// 自上次刷新以来已主动发送的条数。
    pub(crate) sends_since_refresh: usize,
}

#[cfg(feature = "ilink")]
impl IlinkContact {
    /// context_token 是否仍在新鲜窗口内（时效 + 次数上限）。
    pub(crate) fn is_fresh(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        max_age_hours: i64,
    ) -> bool {
        let age = chrono::Duration::max(
            now.signed_duration_since(self.observed_at),
            chrono::Duration::zero(),
        );
        age < chrono::Duration::hours(max_age_hours)
            && self.sends_since_refresh < ILINK_MAX_SENDS_PER_REFRESH
    }
}

/// 获取全局推送注册表（进程级单例）。
pub(crate) fn shared_push() -> &'static PushChannels {
    static PUSH: OnceLock<PushChannels> = OnceLock::new();
    PUSH.get_or_init(PushChannels::default)
}

/// 把通道报文里的消息时间戳（Unix 秒或毫秒）归一化为 RFC3339 世界时字符串。
/// Telegram `Message.date` 为秒级；iLink 类微信报文常为毫秒级。None/非法值返回 None。
pub(crate) fn ts_to_rfc3339(ts: Option<i64>) -> Option<String> {
    let ts = ts?;
    let secs = if ts > 1_000_000_000_000 { ts / 1000 } else { ts };
    chrono::DateTime::from_timestamp(secs, 0).map(|t| t.to_rfc3339())
}

/// 主动外发的具体实现：转译到具体通道派发。
///
/// 无状态、只读 [`shared_push`] 全局注册表与配置；由 bin 接线时注入主动引擎，
/// 延续会话则直接由各 relay 构造零成本实例传入。
#[derive(Default)]
pub struct ChannelsPusher;

impl ChannelsPusher {
    /// 该通道当前是否具备外发条件（电报已连通 / iLink token 仍在新鲜窗口）。
    pub fn pushable(&self, channel: &str, external_id: &str) -> bool {
        match channel {
            "telegram" => {
                if crate::config::Config::get()
                    .telegram_bot_token
                    .trim()
                    .is_empty()
                {
                    return false;
                }
                shared_push()
                    .telegram_ready
                    .load(std::sync::atomic::Ordering::Relaxed)
            }
            #[cfg(feature = "ilink")]
            "ilink" => ilink_has_fresh_token(external_id),
            #[cfg(not(feature = "ilink"))]
            "ilink" => false,
            other => {
                tracing::warn!(channel = other, "未知通道，无法主动推送");
                false
            }
        }
    }

    /// 外发一条文本消息（异步派发，内部处理各通道协议）。
    pub async fn push(
        &self,
        channel: String,
        external_id: String,
        text: String,
    ) -> std::result::Result<(), String> {
        #[cfg(feature = "telegram")]
        if channel == "telegram" {
            let chat_id = external_id
                .parse::<i64>()
                .map_err(|_| "telegram 外部 id 非数字".to_string())?;
            return telegram::push_send(chat_id, &text).await;
        }
        #[cfg(not(feature = "telegram"))]
        if channel == "telegram" {
            tracing::warn!("telegram 通道未启用");
            return Err("telegram 通道未启用".to_string());
        }
        #[cfg(feature = "ilink")]
        if channel == "ilink" {
            return ilink::push_send(&external_id, &text).await;
        }
        #[cfg(not(feature = "ilink"))]
        if channel == "ilink" {
            tracing::warn!("ilink 通道未启用");
            return Err("ilink 通道未启用".to_string());
        }
        tracing::warn!(channel = channel, "暂不支持主动推送的通道");
        Err(format!("未知通道 {channel}"))
    }
}

#[cfg(feature = "ilink")]
fn ilink_has_fresh_token(external_id: &str) -> bool {
    let cfg = crate::config::Config::get();
    let reg = shared_push().ilink.lock().unwrap_or_else(|p| p.into_inner());
    reg.as_ref()
        .and_then(|r| r.contacts.get(external_id))
        .map(|c| c.is_fresh(chrono::Utc::now(), cfg.ilink_context_max_age_hours))
        .unwrap_or(false)
}

/// 把长文本按字符上限切分为多段（优先在换行处断开，中文按字符计数）。
/// 各渠道发送接口都有单条长度限制，统一用该函数分片。
pub fn chunk_text(text: &str, limit: usize) -> Vec<String> {
    if text.is_empty() || limit == 0 {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![text.to_string()]
        };
    }
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.chars().count() <= limit {
            chunks.push(rest.to_string());
            break;
        }
        // 前 limit 个字符的字节结束位置（char_indices 保证落在字符边界）。
        let mut end = rest.len();
        for (i, _) in rest.char_indices() {
            if rest[..i].chars().count() >= limit {
                end = i;
                break;
            }
        }
        // 优先在换行处断开。
        if let Some(nl) = rest[..end].rfind('\n') {
            end = nl + 1;
        }
        chunks.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_short_text_untouched() {
        assert_eq!(chunk_text("hello", 4096), vec!["hello".to_string()]);
        assert!(chunk_text("", 10).is_empty());
    }

    #[test]
    fn chunks_long_text_within_limit() {
        let text = "a".repeat(9000);
        let chunks = chunk_text(&text, 4096);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunks_prefer_newline_and_keep_cjk() {
        // 换行在首段范围内 → 首段应在换行处断开
        let text = format!("{}\n{}", "a".repeat(4000), "b".repeat(4000));
        let chunks = chunk_text(&text, 4096);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), text);
        assert!(chunks[0].ends_with('\n'));

        // 中文按字符计数，不断开字符
        let cjk = "你好".repeat(3000);
        let chunks = chunk_text(&cjk, 4096);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), cjk);
    }
}
