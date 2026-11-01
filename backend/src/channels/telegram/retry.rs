//! Telegram retry 子模块。

use super::*;

/// 重试扫表周期（秒）：每隔多久醒来一次扫 `pending_retry` 表。
pub(super) const SWEEP_INTERVAL_SECS: u64 = 15;
/// 失败轮次的最大重试次数（含首次失败后的重试）。
pub(super) const MAX_RETRY_ATTEMPTS: i64 = 5;
/// 持久重试退避步长（秒），按"已失败次数"索引（从 1 起），用尽后保持最后一步长。
pub(super) const RETRY_BACKOFF_SECS: &[u64] = &[30, 120, 600, 1800];
/// 超时轮次下次重试的最小间隔（秒）：须大于单次 LLM 调用上限（genai 侧 `CHAT_TIMEOUT`=300s），
/// 否则被 `AI_DEADLINE_SECS` 超时打断的旧阻塞任务可能仍在跑，会与新重试并发写同一会话。
pub(super) const RETRY_TIMEOUT_GRACE_SECS: u64 = 360;
/// 本轮对话失败（或超时）时把"待重试"落库。失败**不再向用户发声**——原兜底话术
/// "刚才走神了，再发一次试试。"会误导用户重发，而用户消息其实已在对话流水线里落库；
/// 这里改为入队，由 [`run_retry_sweep`] 后续静默重试，成功才把回复送达。
///
/// 重试目标**优先取更新的那条用户消息**（本轮跑着的时候用户又说了话）：重跑会用
/// `exclude_id` 把历史截到触发消息为止（见 `context::fetch_history`），若仍以本轮触发
/// 消息为水位线，插话内容会被截掉、模型看不见——而用户最后说的那句才是最该回应的。
///
/// `timeout` 为真表示本轮是被 `AI_DEADLINE_SECS` 超时打断——其内部的阻塞任务可能仍在跑，
/// 故下次重试时刻推到超时之后，避免与仍在运行的旧任务并发写同一会话。
pub(super) fn schedule_retry(
    session: &Sessions,
    session_id: &str,
    chat_id: i64,
    content: &str,
    user_time: Option<String>,
    user_message_id: &str,
    timeout: bool,
) {
    // 会话里出现了更新的用户消息：重跑它，本轮触发消息退化成普通历史。
    let (content, user_time, user_message_id) =
        match session.newer_user_message(session_id, user_message_id) {
            Ok(Some(m)) => (m.content, Some(m.created_at), m.id),
            Ok(None) => (content.to_string(), user_time, user_message_id.to_string()),
            Err(e) => {
                tracing::warn!(error = %e, chat_id, "读取更新消息失败，按本轮触发消息重试");
                (content.to_string(), user_time, user_message_id.to_string())
            }
        };
    // 超时轮次要把下次重试推到旧阻塞任务收尾之后；普通失败按首档退避。
    let delay_secs = if timeout {
        RETRY_TIMEOUT_GRACE_SECS as i64
    } else {
        RETRY_BACKOFF_SECS[0] as i64
    };
    if let Err(e) = session.enqueue_retry_for(
        session_id,
        &chat_id.to_string(),
        &user_message_id,
        &content,
        user_time,
        delay_secs,
    ) {
        tracing::error!(error = %e, chat_id, "待重试入队失败");
    }
}

/// 计算第 `attempts` 次失败后的下次重试时刻（attempts 从 1 起，等于已重试次数 + 1）。
///
/// 退避步长按 [`RETRY_BACKOFF_SECS`] 索引，超过后保持最后一步长；超时场景至少退到
/// [`RETRY_TIMEOUT_GRACE_SECS`] 之后，确保旧阻塞任务已收尾、不与新重试并发。
pub(super) fn next_backoff(attempts: i64, now: DateTime<Utc>, timeout: bool) -> DateTime<Utc> {
    let base = RETRY_BACKOFF_SECS
        .get((attempts - 1) as usize)
        .copied()
        .unwrap_or(*RETRY_BACKOFF_SECS.last().unwrap_or(&30));
    let secs = if timeout {
        base.max(RETRY_TIMEOUT_GRACE_SECS)
    } else {
        base
    };
    now + chrono::Duration::seconds(secs as i64)
}
/// 失败轮次重试扫表：周期性扫描 `pending_retry`，把到期且未达上限的轮次重新跑一遍。
///
/// - 复用已落库的用户消息（同一 `user_message_id`），不重复落库；
/// - **成功**才由工具循环经 [`TgDeliver`] 把回复送达用户，然后从队列删除；
/// - **失败**推进退避（写回 `next_attempt_at`）等下一轮；耗尽则静默放弃。
///
/// 无论成功/放弃，用户消息始终留在 `messages` 时间线，不丢上下文；失败也**不向用户发
/// 任何话术**（原"刚才走神了"会误导重发，而消息其实已落库）。
pub async fn run_retry_sweep(chat: Arc<Agent>, session: Arc<Sessions>) {
    let token = Config::get().telegram_bot_token.trim().to_string();
    if token.is_empty() {
        return;
    }
    let client = match telegram_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "重试扫表无法构建 Telegram client，退出");
            return;
        }
    };
    let base = api_base(&token);
    let media_url = Config::get().media_gateway_url.clone();
    let media_timeout = crate::core::settings::snapshot().media_gateway_timeout;

    let mut ticker = tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
    loop {
        ticker.tick().await;
        let now = Utc::now();
        let due = match session.due_retries(now) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "读取待重试队列失败，下一周期重试");
                continue;
            }
        };
        // 逐行顺序处理（每行 `await` 到结果），天然不会自行并发；跨轮次对同一行的重叠由
        // `next_attempt_at` 退避兜住——超时轮次的下次重试被推到 `RETRY_TIMEOUT_GRACE_SECS`
        // 之后（大于单次 LLM 调用上限），原阻塞任务此时已收尾，不会与新重试并发。
        for r in due {
            let chat_id = match r.chat_id.parse::<i64>() {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(error = %e, id = %r.chat_id, "pending_retry.chat_id 非法，跳过");
                    continue;
                }
            };
            // 会话里已经出现更新的用户消息 = 这条重试的目标过时了（用户后来又说了一句，
            // 那一轮自己会跑/已经跑了）。再重试只会答上一句旧话，还可能把新话重复回一遍，
            // 直接出队：用户消息仍在时间线里，上下文不丢。
            match session.newer_user_message(&r.session_id, &r.user_message_id) {
                Ok(Some(_)) => {
                    tracing::info!(chat_id, "会话里已有更新的用户消息，放弃这条过时重试");
                    let _ = session.remove_retry(&r.id);
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, chat_id, "读取会话最新消息失败，仍按原目标重试")
                }
            }

            // 会话正跑着轮次时不并跑：本轮扫表跳过它，下一周期再看（对方那一轮很可能
            // 已经把这句回掉了，届时会被上面的"过时"判据出队）。
            let Some(ticket) = session.begin_turn(&r.session_id) else {
                tracing::info!(chat_id, "本会话已有轮次在跑，重试推迟到下一周期");
                continue;
            };
            // 凭证持到本行处理结束，让插话机制（同会话新消息并进本轮上下文）照常生效。
            let _ticket = ticket;

            // 同一次扫表内顺序处理，每个轮次用独立投递工人即时送达回复。
            let (deliver_tx, deliver_rx) = tokio::sync::mpsc::unbounded_channel();
            let deliverer = TgDeliver::new(deliver_tx);
            let worker = tokio::spawn(deliver_worker(
                client.clone(),
                base.clone(),
                chat_id,
                media_url.clone(),
                media_timeout,
                deliver_rx,
            ));
            let handled = tokio::time::timeout(
                Duration::from_secs(super::super::AI_DEADLINE_SECS),
                handle_text(
                    chat.clone(),
                    session.clone(),
                    chat_id,
                    &r.content,
                    r.user_time.clone(),
                    deliverer.clone(),
                    // 重试目标消息早已落库，按 id 复用同一行，不重复入库。
                    r.user_message_id.clone(),
                ),
            )
            .await;
            // 收尾：丢弃投递口、abort 工人（超时那轮的内核任务可能仍挂着，不 await）。
            // 失败分支要按"本轮已送达几片"决定还重不重试，先把计数取出来再丢投递口。
            let delivered = deliverer.delivered_count();
            drop(deliverer);
            worker.abort();

            match handled {
                Ok(Ok(_)) => {
                    if let Err(e) = session.remove_retry(&r.id) {
                        tracing::error!(error = %e, chat_id, "重试成功但删除队列项失败");
                    } else {
                        tracing::info!(chat_id, "待重试轮次重试成功，回复已送达，出队");
                    }
                }
                Ok(Err(e)) => {
                    if !should_retry_after_failure(delivered) {
                        // 这次重试已经把一部分话说出去了：再跑一遍就是重复同样的内容，收手出队。
                        tracing::warn!(
                            error = %e,
                            chat_id,
                            delivered,
                            "重试中途失败，但已有内容送达：不再重试，出队"
                        );
                        let _ = session.remove_retry(&r.id);
                    } else {
                        let attempts = r.attempts + 1;
                        if attempts >= MAX_RETRY_ATTEMPTS {
                            tracing::warn!(
                                error = %e,
                                chat_id,
                                attempts,
                                "重试仍失败且已达上限，静默放弃（用户消息保留在时间线）"
                            );
                            let _ = session.remove_retry(&r.id);
                        } else {
                            let next = next_backoff(attempts, Utc::now(), false);
                            if let Err(me) = session.mark_retry_attempted(&r.id, next) {
                                tracing::error!(error = %me, chat_id, "推进重试退避失败");
                            } else {
                                tracing::warn!(
                                    error = %e,
                                    chat_id,
                                    attempts,
                                    "重试仍失败，已退避，等待下一轮扫表"
                                );
                            }
                        }
                    }
                }
                Err(_) => {
                    if !should_retry_after_failure(delivered) {
                        tracing::warn!(
                            chat_id,
                            delivered,
                            "重试超时，但已有内容送达：不再重试，出队"
                        );
                        let _ = session.remove_retry(&r.id);
                    } else {
                        let attempts = r.attempts + 1;
                        if attempts >= MAX_RETRY_ATTEMPTS {
                            tracing::warn!(
                                chat_id,
                                attempts,
                                "重试超时且已达上限，静默放弃（用户消息保留在时间线）"
                            );
                            let _ = session.remove_retry(&r.id);
                        } else {
                            // 超时：旧阻塞任务可能仍跑，下次重试时刻推到超时之后。
                            let next = next_backoff(attempts, Utc::now(), true);
                            if let Err(me) = session.mark_retry_attempted(&r.id, next) {
                                tracing::error!(error = %me, chat_id, "推进重试退避失败");
                            } else {
                                tracing::warn!(
                                    chat_id,
                                    attempts,
                                    "重试超时，已退避，等待下一轮扫表"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
