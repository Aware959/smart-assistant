//! 发送派发：按候选通道经具体推送实现（[`crate::channels::ChannelsPusher`]）外发，成功后回写状态与历史。

use crate::channels::ChannelsPusher;
use crate::core::intent::IntentCandidate;
use crate::core::intent::IntentTracker;
use crate::core::session::Sessions;

/// 通道当前是否可能把消息送出去（telegram 已连接 / iLink 仍有新鲜 token）。
/// 用于在触发 LLM 决策之前先剔除不可推的用户，省下无谓的模型调用。
pub fn pushable(pusher: &ChannelsPusher, cand: &IntentCandidate) -> bool {
    pusher.pushable(&cand.channel, &cand.external_id)
}

/// 意图开口（主动推送，由延迟等待引擎触发）：发送并计入每日开口配额与历史。
/// 等价于 [`send_message`] 的 `count_as_intent = true` 版本。
pub async fn dispatch(
    session: &Sessions,
    tracker: &IntentTracker,
    pusher: &ChannelsPusher,
    cand: &IntentCandidate,
    text: &str,
) -> bool {
    send_message(session, tracker, pusher, cand, text, true).await
}

/// 发送一条消息并回写历史；`count_as_intent` 为 true 时同时计入每日主动配额。
///
/// - 延续会话中的追加句是"对话"，不计入主动配额；
/// - 延迟等待引擎的主动发起才计入配额。
pub async fn send_message(
    session: &Sessions,
    tracker: &IntentTracker,
    pusher: &ChannelsPusher,
    cand: &IntentCandidate,
    text: &str,
    count_as_intent: bool,
) -> bool {
    match pusher
        .push(
            cand.channel.clone(),
            cand.external_id.clone(),
            text.to_string(),
        )
        .await
    {
        Ok(()) => {
            record_sent(session, tracker, cand, text, count_as_intent);
            tracing::info!(
                channel = %cand.channel,
                user = %cand.external_id,
                intent = %count_as_intent,
                "消息发送成功"
            );
            true
        }
        Err(e) => {
            tracing::info!(
                channel = %cand.channel,
                user = %cand.external_id,
                error = %e,
                "消息未发送"
            );
            false
        }
    }
}

/// 成功后把消息写进该用户的历史（避免上下文断裂），并按需刷新意图状态。
/// 统一走服务方法（会话落库 / 意图状态追踪配额），不再直连 db / 通道。
fn record_sent(
    session: &Sessions,
    tracker: &IntentTracker,
    cand: &IntentCandidate,
    text: &str,
    count_as_intent: bool,
) {
    if let Err(e) = session.add_message(&cand.session_id, "assistant", text) {
        tracing::warn!(error = %e, "消息写回历史失败");
    }
    if count_as_intent {
        if let Err(e) = tracker.record_intent_send(&cand.channel, &cand.external_id, &cand.session_id) {
            tracing::warn!(error = %e, "意图状态记录失败");
        }
    }
}