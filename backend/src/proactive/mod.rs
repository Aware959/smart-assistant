//! 主动陪伴引擎：像真人一样在不规律的时间主动给用户发消息。
//!
//! 频率控制（DB 状态簿 + 随机间隔 + 安静时段 + 每日上限）：见 [`crate::db::proactive`]；
//! 节奏计算：见 [`scheduler`]；开口决策（LLM）：见 [`decider`]；通道派发：见 [`sender`]。
//!
//! 与通道的关系：Telegram 长连通后可随时推送；iLink 依赖用户最近入站刷新的
//! `context_token`（窗口约束见 [`crate::channels`]），窗口内不可推的用户在决策前
//! 就被剔除。每轮周期最多主动联系一个用户，其余留到下一轮，避免连环轰炸。

pub mod continuation;
pub mod context;
pub mod decider;
pub mod scheduler;
pub mod sender;

use std::sync::Arc;

use crate::config::Config;
use crate::host::Host;
use crate::Assistant;

/// 每轮最多尝试决策的候选数（其余留到后续轮次）。
const MAX_CANDIDATES_PER_CYCLE: usize = 5;

/// 主动引擎入口：随机间隔循环直到进程退出。
pub async fn run(assistant: Arc<Assistant>) {
    if !Config::get().proactive_enabled {
        tracing::info!("主动陪伴未启用（设置 PROACTIVE_ENABLED=1 开启）");
        return;
    }
    tracing::info!("主动陪伴引擎已启动");
    loop {
        let idle = scheduler::next_sleep();
        tracing::debug!(next_in_secs = idle.as_secs(), "等待下一轮主动评估");
        tokio::time::sleep(idle).await;
        run_once(&assistant).await;
    }
}

/// 单轮评估：筛出可推送的候选，逐个询问 LLM 是否开口，至多发出一条。
async fn run_once(assistant: &Assistant) {
    let round_start = std::time::Instant::now();
    // 竞态兜底：等待期间可能恰好跨入安静时段。
    if scheduler::in_quiet_hours(chrono::Local::now()) {
        tracing::info!("当前处于安静时段，跳过本轮评估");
        return;
    }

    let candidates = {
        let db = assistant.inner_db();
        crate::db::proactive::eligible_candidates(&db, chrono::Utc::now())
    };
    let candidates = match candidates {
        Ok(list) => list,
        Err(e) => {
            tracing::error!(error = %e, "候选过滤失败");
            return;
        }
    };
    if candidates.is_empty() {
        tracing::debug!(elapsed_ms = round_start.elapsed().as_millis() as u64, "本轮无可用候选");
        return;
    }
    tracing::info!(n = candidates.len(), elapsed_ms = round_start.elapsed().as_millis() as u64, "主动评估：候选已筛出");

    for cand in candidates.iter().take(MAX_CANDIDATES_PER_CYCLE) {
        if !sender::pushable(cand) {
            continue;
        }
        let Some(text) = propose(assistant, cand).await else {
            continue;
        };
        tracing::info!(user = %cand.external_id, elapsed_ms = round_start.elapsed().as_millis() as u64, "主动评估：决策开口，准备派发");
        sender::dispatch(assistant, cand, &text).await;
        tracing::info!(user = %cand.external_id, elapsed_ms = round_start.elapsed().as_millis() as u64, "主动评估：派发完成");
        return;
    }
}

/// 先组装上下文，再问 LLM 要不要开口；返回待发送的内容（不开口则为 None）。
async fn propose(assistant: &Assistant, cand: &crate::db::proactive::ProactiveCandidate) -> Option<String> {
    let step_start = std::time::Instant::now();
    // 每个 DB 读取独立短作用域，守卫用完即释放。
    // 切勿在持 `inner_db()` 守卫期间再调 `world_snapshot_text`（它内部会再次
    // 锁同一把非重入的 std Mutex，Proactive 引擎整个挂死，进而拖垮全进程）。
    let recent = {
        let db = assistant.inner_db();
        context::recent_history(&db, &cand.session_id, 8)
    };
    let rec = {
        let query = recent.chars().take(200).collect::<String>();
        if query.trim().is_empty() {
            String::new()
        } else {
            let db = assistant.inner_db();
            context::recall(&db, &query)
        }
    };
    let world = assistant.world_snapshot_text(&cand.session_id);
    tracing::info!(
        user = %cand.external_id,
        elapsed_ms = step_start.elapsed().as_millis() as u64,
        rec_chars = rec.chars().count(),
        "主动评估：上下文组装完成（recent/recall/world）"
    );

    // LLM 调用是同步阻塞的（genai 全局 runtime），挪到阻塞线程池避免卡住异步任务。
    let llm = assistant.chat_llm();
    let decision = tokio::task::spawn_blocking(move || decider::decide(&*llm, &recent, &rec, &world))
        .await
        .unwrap_or_default();

    if !decision.speak {
        if let Some(reason) = &decision.reason {
            tracing::debug!(user = %cand.external_id, reason, "LLM 判断本轮不开口");
        }
        tracing::info!(user = %cand.external_id, elapsed_ms = step_start.elapsed().as_millis() as u64, "主动评估：决策完成（不开口）");
        return None;
    }
    tracing::info!(user = %cand.external_id, elapsed_ms = step_start.elapsed().as_millis() as u64, "主动评估：决策完成（开口）");
    decision.message
}