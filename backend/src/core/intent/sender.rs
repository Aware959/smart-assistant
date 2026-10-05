//! 发送派发：按候选通道经具体推送实现（[`crate::channels::ChannelsPusher`]）外发，成功后回写历史。

use crate::channels::ChannelsPusher;
use crate::core::intent::IntentCandidate;
use crate::core::session::Sessions;

/// 通道当前是否可能把消息送出去（telegram 已连接 / iLink 仍有新鲜 token）。
/// 用于在触发 LLM 决策之前先剔除不可推的用户，省下无谓的模型调用。
pub fn pushable(pusher: &ChannelsPusher, cand: &IntentCandidate) -> bool {
    pusher.pushable(&cand.channel, &cand.external_id)
}

/// 意图开口（主动推送，由延迟等待引擎触发）：发送成功后写入该用户历史。
///
/// 历史里落一条 `assistant` 消息就够了——防连环轰炸的间隔门槛直接取最后一条
/// assistant 消息时间，不区分"回复"与"主动开口"，因此没有额外状态要维护。
pub async fn dispatch(
    session: &Sessions,
    pusher: &ChannelsPusher,
    cand: &IntentCandidate,
    text: &str,
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
            record_sent(session, cand, text);
            tracing::info!(
                channel = %cand.channel,
                user = %cand.external_id,
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

/// 成功后把消息写进该用户的历史（避免上下文断裂）。
fn record_sent(session: &Sessions, cand: &IntentCandidate, text: &str) {
    if let Err(e) = session.add_message(&cand.session_id, "assistant", text) {
        tracing::warn!(error = %e, "消息写回历史失败");
    }
}
