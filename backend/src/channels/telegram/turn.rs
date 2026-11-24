//! Telegram turn 子模块。

use super::*;

/// 用户发来消息时解析/确认会话映射，返回会话 id（映射建不起来则 None）。
///
/// 主动开口的静默窗口不再由状态表维护——它直接取 `messages` 里最后一条用户消息的
/// 真实发出时刻，所以这里只剩"确保映射存在"这一件事（否则该用户进不了候选集）；
/// session id 顺手交给调用方，后续落库与起轮次都要用。
pub(super) fn touch_user_reply(
    session: &Sessions,
    external_id: &str,
    _sent_at: Option<DateTime<Utc>>,
) -> Option<String> {
    match session.resolve(CHANNEL, external_id) {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(error = %e, "解析会话映射失败");
            None
        }
    }
}
/// 处理一条 TG 对话消息，返回本轮对话结果。
///
/// 用户消息**在通道侧收到时就已落库**（见 [`run_loop`]），这里按 id 复用、不再重复入库：
/// 落库先于起轮次，是"运行中插话"成立的前提——一轮在跑时新到的消息先落库、由那一轮在
/// 迭代边界取用（见 `core::agent::ingest_steers`），而不是排队等这轮跑完。
///
/// 回复在**本轮内即时投递**（见 [`TgDeliver`]）：内核在工具循环里逐片段投递，骰子点数
/// 也在这条路径上拿回并回填给模型。只负责产出回复；记忆由模型在本轮内自行决定是否写入
/// （`remember` 工具）。命令类消息（`/start` / `/new`）不走到这里，见 [`command_reply`]。
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_text(
    chat: Arc<Agent>,
    session: Arc<Sessions>,
    chat_id: i64,
    text: &str,
    user_time: Option<String>,
    deliverer: TgDeliver,
    user_message_id: String,
) -> Result<ChatOutput, String> {
    let session_id = session
        .resolve(CHANNEL, &chat_id.to_string())
        .map_err(|e| e.to_string())?;
    let text = text.to_string();
    // 同步的对话流水线要在阻塞线程池里跑（api/chat.rs 同理）；投递口一并移进去，
    // 内核在工具循环里即时发送、当场拿回骰子点数。投递要阻塞等回执，只有在这个
    // 阻塞线程上才安全——放到 runtime 工人线程上会把调度线程占死。
    let worker = deliverer.clone();
    tokio::task::spawn_blocking(move || {
        let input = ChatInput {
            message: text,
            session_id: Some(session_id),
            history: Vec::new(),
            history_count: None,
            user_time,
        };
        // 槽位形参只服务于 Web/iLink 那条"由 `chat_stream` 自行入库"的老路，Telegram
        // 这边消息已落库、id 由调用方直接持有，传空槽即可。
        chat.chat_stream(
            &input,
            move |reply| worker.deliver(reply),
            std::sync::Arc::new(std::sync::Mutex::new(None)),
            Some(user_message_id),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("对话任务失败: {e}"))?
}

/// 起一轮对话：投到独立任务里跑，**长轮询不等它**。
///
/// 这是"agent 跑着也能插话"在通道侧的另一半：轮询循环必须一直转着去收新消息，否则用户
/// 的第二句话要等这一轮跑完、长轮询被重新调用才拿得到。轮次名额由凭证 [`TurnTicket`]
/// 登记，同一会话同时只跑一轮（[`Sessions::begin_turn`]）；期间新到的消息照常落库，
/// 由这一轮在迭代边界并进上下文（见 `core::agent` 的插话机制）。
///
/// 失败/超时的处理与过去一致：一条都没送达才入队静默重试（见 [`schedule_retry`]）；
/// 已经有内容送达就不再重跑（重跑会把同样的话再说一遍）。后一种情况下，本轮没赶上的
/// 插话不会丢——它就在时间线里，用户下一条消息进来时一并被看到。
#[allow(clippy::too_many_arguments)] // 会话、连接、消息、凭证各占一个，都是起一轮必需的输入
pub(super) fn spawn_turn(
    chat: Arc<Agent>,
    session: Arc<Sessions>,
    client: reqwest::Client,
    base: String,
    chat_id: i64,
    session_id: String,
    text: String,
    user_time: Option<String>,
    user_message_id: String,
    ticket: TurnTicket,
) {
    tokio::spawn(async move {
        // 凭证必须在任务内持有到结束（含超时）：任务一结束它就析构，会话随之放行下一轮。
        let _ticket = ticket;
        // 本轮产出的是**一组独立消息**（模型自己决定说几句），由内核在工具循环里
        // 逐条即时投递：投递口把片段交给同一 runtime 上的工人去发，内核对每个片段
        // 阻塞等回执，顺序天然不乱，骰子点数也在这条路径上当场拿回并回填给模型。
        let (deliver_tx, deliver_rx) = tokio::sync::mpsc::unbounded_channel();
        let deliverer = TgDeliver::new(deliver_tx);
        let worker = tokio::spawn(deliver_worker(client, base, chat_id, deliver_rx));
        let started = std::time::Instant::now();

        let handled = tokio::time::timeout(
            Duration::from_secs(super::super::AI_DEADLINE_SECS),
            handle_text(
                chat,
                session.clone(),
                chat_id,
                &text,
                user_time.clone(),
                deliverer.clone(),
                user_message_id.clone(),
            ),
        )
        .await;

        match handled {
            Ok(Ok(out)) => {
                tracing::info!(
                    chat_id,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    messages = out.messages.len(),
                    memory_written = out.memory.is_some(),
                    "handle_text 返回（回复已即时投递）"
                );
            }
            Ok(Err(e)) => {
                let delivered = deliverer.delivered_count();
                if should_retry_after_failure(delivered) {
                    tracing::error!(
                        error = %e,
                        chat_id,
                        "对话失败，入队待重试（不再向用户发声）"
                    );
                    schedule_retry(
                        &session,
                        &session_id,
                        chat_id,
                        &text,
                        user_time,
                        &user_message_id,
                        false,
                    );
                } else {
                    tracing::warn!(
                        error = %e,
                        chat_id,
                        delivered,
                        "对话失败，但本轮已有内容送达：不重试（重跑会把同样的话再说一遍）"
                    );
                }
            }
            Err(_) => {
                let delivered = deliverer.delivered_count();
                if should_retry_after_failure(delivered) {
                    tracing::warn!(
                        chat_id,
                        deadline_secs = super::super::AI_DEADLINE_SECS,
                        "单轮对话超时，入队待重试（不再向用户发声）"
                    );
                    schedule_retry(
                        &session,
                        &session_id,
                        chat_id,
                        &text,
                        user_time,
                        &user_message_id,
                        true,
                    );
                } else {
                    tracing::warn!(
                        chat_id,
                        delivered,
                        deadline_secs = super::super::AI_DEADLINE_SECS,
                        "单轮对话超时，但本轮已有内容送达：不重试（重跑会把同样的话再说一遍）"
                    );
                }
            }
        }

        // 收尾：丢弃投递口让工人自然退出，再兜底 abort（超时那轮的内核任务可能
        // 还挂着，不能再等它，也不能让工人一直空转）。**不 await 工人**——超时后
        // 那个仍存活的 `spawn_blocking` 会一直持有它的发送端。
        drop(deliverer);
        worker.abort();
    });
}
