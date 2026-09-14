//! 世界引擎：让 AI 真正"活着"，而不只是在对话时被唤醒。
//!
//! 一个在服务进程里最小间隔 60 秒的心跳循环，每跳推进三件事：
//! 1. 情绪向基线回归（[`emotion`]）；
//! 2. 所有关系随时间降温（[`relation`]）；
//! 3. 时段/日期切换时续写今日叙事（[`narrative`]，LLM 调用走阻塞线程池）。
//!
//! 与主动推送解耦：无论 `PROACTIVE_ENABLED` 是否开启，AI 的世界都持续前行；
//! 主动开口只是世界观被人看到的那一面。

pub mod emotion;
pub mod narrative;
pub mod relation;
pub mod self_state;

#[cfg(any(feature = "telegram", feature = "ilink"))]
use std::sync::Arc;

#[cfg(any(feature = "telegram", feature = "ilink"))]
use crate::Assistant;

/// 世界心跳间隔（秒）。
#[cfg(any(feature = "telegram", feature = "ilink"))]
const TICK_SECS: u64 = 60;

/// 世界引擎入口：常驻循环直到进程退出。
#[cfg(any(feature = "telegram", feature = "ilink"))]
pub async fn run(assistant: Arc<Assistant>) {
    tracing::info!("世界引擎已启动（AI 内部状态持续推进）");
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(TICK_SECS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        tick(assistant.clone()).await;
    }
}

/// 单跳推进：快速的状态衰减在前台做，叙事续写交给阻塞线程池。
#[cfg(any(feature = "telegram", feature = "ilink"))]
async fn tick(assistant: Arc<Assistant>) {
    // 1. 情绪：按距上次的经过时间向基线回归。
    {
        let db = assistant.inner_db();
        if let Err(e) = self_state::tick_mood(&db) {
            tracing::debug!(error = %e, "情绪推进失败");
        }
    }

    // 2. 关系：全部按距上次的经过时间降温。
    {
        let db = assistant.inner_db();
        if let Err(e) = relation::tick_decay(&db) {
            tracing::debug!(error = %e, "关系降温失败");
        }
    }

    // 3. 叙事：需要时才触发（LLM 阻塞 → spawn_blocking）。
    let go = {
        let db = assistant.inner_db();
        narrative::should_advance(&db).unwrap_or(false)
    };
    if !go {
        return;
    }
    match tokio::task::spawn_blocking(move || narrative::advance(&assistant)).await {
        Ok(inner) => {
            if let Err(e) = inner {
                tracing::warn!(error = %e, "今日叙事推进失败");
            }
        }
        Err(join) => tracing::warn!(error = %join, "叙事阻塞任务被取消"),
    }
}