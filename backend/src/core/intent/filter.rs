//! 频率规则过滤：筛出"此刻值得考虑主动开口"的用户（[`filter_eligible`]）。
//!
//! 本模块只保留领域规则——过滤逻辑与候选映射；SQL 层
//! （[`crate::db::channel::list_proactive_rows`]）只负责按消息时间线取行，不做业务判断。
//!
//! 规则全是机械门槛，不写进提示词：是否开口由 LLM 结合对话自行判断（见 [`crate::core::intent::speak`]）。

use chrono::{DateTime, Duration, Utc};

use crate::config::Config;
use crate::core::intent::IntentCandidate;
use crate::db::channel::ProactiveRow;

/// 频率规则过滤：返回"当前值得考虑主动开口"的用户。
///
/// 由调用方再按通道可推性（telegram 随时可推 / ilink 需新鲜 context_token）二次筛选。
pub fn filter_eligible(rows: Vec<ProactiveRow>, now: DateTime<Utc>) -> Vec<IntentCandidate> {
    let cfg = Config::get();
    let min_silence = Duration::hours(cfg.intent_min_silence_hours);
    let max_idle = Duration::days(cfg.intent_max_idle_days);
    let min_gap = Duration::minutes(cfg.intent_min_minutes);

    rows.into_iter()
        .map(|r| IntentCandidate {
            channel: r.channel,
            external_id: r.external_id,
            session_id: r.session_id,
            last_user_reply_at: r.last_user_at,
            last_assistant_at: r.last_assistant_at,
        })
        .filter(|c| {
            let Some(reply_at) = c.last_user_reply_at else {
                return false;
            };
            let quiet = non_negative(now.signed_duration_since(reply_at));
            if quiet < min_silence {
                return false;
            }
            if quiet > max_idle {
                return false;
            }
            if let Some(p) = c.last_assistant_at {
                if non_negative(now.signed_duration_since(p)) < min_gap {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// 时钟回拨等导致的负时长统一按 0 处理。
fn non_negative(d: Duration) -> Duration {
    Duration::max(d, Duration::zero())
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod filter_tests;
