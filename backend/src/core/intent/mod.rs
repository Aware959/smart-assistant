//! 意图判定引擎：由 AI 侧决定"此刻是否开口 / 是否续话"，像真人一样不规律地主动联系。
//!
//! 频率控制（随机间隔 + 安静时段 + 静默/流失/最近 assistant 间隔门槛）：见 [`pace`] 与
//! [`filter`]；开口决策（走与对话同一套工具循环，见 [`crate::core::agent::Agent::proactive_stream`]）；
//! 通道派发：见 [`sender`]。
//!
//! 候选状态不落独立状态表：直接从 `channel_sessions` + `messages` 时间线派生
//! （见 [`crate::db::channel::list_proactive_rows`]），因此进程重启也不会丢状态。
//!
//! **开口决策不再是一次性的 JSON 判定**：过去用一次 `complete` 让模型吐出
//! "说不说 + 说什么"，固定召回几条记忆、也拿不到当前时间。现在改为复用对话的
//! 工具循环——模型能按需 `recall_memory`、能调 `get_now`，提示词里也直接注入了当前
//! 时间与最后一次互动时刻；默认沉默靠主动路径的专属提示词约束。
//!
//! 与通道的关系：Telegram 长连通后可随时推送；iLink 依赖用户最近入站刷新的
//! `context_token`（窗口约束见 [`crate::channels`]），窗口内不可推的用户在决策前
//! 就被剔除。每轮周期最多开口联系一个用户，其余留到下一轮，避免连环轰炸。

pub mod filter;
pub mod pace;
pub mod sender;

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::channels::ChannelsPusher;
use crate::config::Config;
use crate::core::agent::Agent;
use crate::core::session::Sessions;

/// 意图候选：某一外部用户（channel × external_id）当前的状态快照。
///
/// 由消息时间线派生、经频率过滤后产出，意图引擎（决策 / 派发）消费；
/// 业务形状归属意图领域，不长期在 SQL 层。
#[derive(Debug, Clone)]
pub struct IntentCandidate {
    pub channel: String,
    pub external_id: String,
    pub session_id: String,
    /// 用户最后一次入站消息时间；从没说过话则为 None。
    pub last_user_reply_at: Option<DateTime<Utc>>,
    /// 最近一条 assistant 消息时间；不区分回复/主动开口，用于防止连环轰炸。
    pub last_assistant_at: Option<DateTime<Utc>>,
}

/// 每轮最多尝试决策的候选数（其余留到后续轮次）。
const MAX_CANDIDATES_PER_CYCLE: usize = 5;

/// 意图引擎入口：随机间隔循环直到进程退出。
///
/// `pusher` 是主动外发能力的具体实现（[`crate::channels::ChannelsPusher`]）。
pub async fn run(chat: Arc<Agent>, session: Arc<Sessions>, pusher: Arc<ChannelsPusher>) {
    if !Config::get().intent_enabled {
        tracing::info!("意图判定未启用（设置 INTENT_ENABLED=1 开启）");
        return;
    }
    tracing::info!("意图判定引擎已启动");
    loop {
        let idle = pace::next_sleep();
        tracing::debug!(next_in_secs = idle.as_secs(), "等待下一轮意图判定");
        tokio::time::sleep(idle).await;
        run_once(chat.clone(), session.as_ref(), pusher.as_ref()).await;
    }
}

/// 单轮评估：筛出可开口的候选，逐个询问 LLM 是否开口，至多说出一条。
async fn run_once(chat: Arc<Agent>, session: &Sessions, pusher: &ChannelsPusher) {
    let round_start = std::time::Instant::now();
    // 竞态兜底：等待期间可能恰好跨入安静时段。
    if pace::in_quiet_hours(chrono::Local::now()) {
        tracing::info!("当前处于安静时段，跳过本轮评估");
        return;
    }

    let candidates = session.proactive_candidates(chrono::Utc::now());
    let candidates = match candidates {
        Ok(list) => list,
        Err(e) => {
            tracing::error!(error = %e, "候选过滤失败");
            return;
        }
    };
    if candidates.is_empty() {
        tracing::debug!(
            elapsed_ms = round_start.elapsed().as_millis() as u64,
            "本轮无可用候选"
        );
        return;
    }
    tracing::info!(
        n = candidates.len(),
        elapsed_ms = round_start.elapsed().as_millis() as u64,
        "意图判定：候选已筛出"
    );

    for cand in candidates.iter().take(MAX_CANDIDATES_PER_CYCLE) {
        if !sender::pushable(pusher, cand) {
            continue;
        }
        let messages = propose(chat.clone(), cand).await;
        if messages.is_empty() {
            continue;
        }
        tracing::info!(user = %cand.external_id, elapsed_ms = round_start.elapsed().as_millis() as u64, "意图判定：决策开口，准备派发");
        sender::dispatch(pusher, cand, &messages).await;
        tracing::info!(user = %cand.external_id, elapsed_ms = round_start.elapsed().as_millis() as u64, "意图判定：派发完成");
        return;
    }
}

/// 走对话同一套工具循环做开口决策：返回待发送的消息（决定不开口则为空）。
///
/// 上下文（最近历史 / 当前时间 / 最后互动时刻）由 [`Agent::proactive_stream`] 内部
/// 组装；记忆不再预召回，改由模型按需调 `recall_memory`。
async fn propose(chat: Arc<Agent>, cand: &IntentCandidate) -> Vec<String> {
    let trace_id = crate::utils::trace::new_id();
    let step_start = std::time::Instant::now();
    tracing::info!(
        target: "trace",
        stage = "intent_context",
        trace_id = %trace_id,
        user = %cand.external_id,
        session_id = %cand.session_id,
        "意图判定：开始决策"
    );

    // LLM 调用是同步阻塞的（genai 全局 runtime），挪到阻塞线程池避免卡住异步任务。
    let session_id = cand.session_id.clone();
    let now = chrono::Utc::now();
    let last_user = cand.last_user_reply_at;
    let last_assistant = cand.last_assistant_at;
    let messages = tokio::task::spawn_blocking(move || {
        chat.proactive_stream(&session_id, now, last_user, last_assistant, |_reply| {
            Ok(None)
        })
    })
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "主动决策任务 join 失败，本轮不开口");
        Ok(Vec::new())
    })
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "主动决策调用失败，本轮不开口");
        Vec::new()
    });

    tracing::info!(
        target: "trace",
        stage = "intent_decision",
        trace_id = %trace_id,
        user = %cand.external_id,
        messages = messages.len(),
        "意图判定：决策完成"
    );
    tracing::info!(
        user = %cand.external_id,
        elapsed_ms = step_start.elapsed().as_millis() as u64,
        messages = messages.len(),
        "意图判定：决策完成"
    );
    messages
}
