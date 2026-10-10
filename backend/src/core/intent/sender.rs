//! 发送派发：按候选通道经具体推送实现（[`crate::channels::ChannelsPusher`]）逐条外发。
//!
//! 历史**不在这里回写**——主动开口的消息由内核的工具循环（[`crate::core::agent`]）
//! 在产出时即已入库，和对话路径一致。这里只负责把已经落库的消息按通道推出去。

use crate::channels::ChannelsPusher;
use crate::core::intent::IntentCandidate;

/// 通道当前是否可能把消息送出去（telegram 已连接 / iLink 仍有新鲜 token）。
/// 用于在触发 LLM 决策之前先剔除不可推的用户，省下无谓的模型调用。
pub fn pushable(pusher: &ChannelsPusher, cand: &IntentCandidate) -> bool {
    pusher.pushable(&cand.channel, &cand.external_id)
}

/// 意图开口（主动推送，由延迟等待引擎触发）：把决策产出的每条消息按顺序推出去。
///
/// 多条消息之间按 [`crate::channels::pacing_delay`] 留出间隔，模拟真人一句接一句的
/// 节奏；某条失败即停止后续（后半句失去上半句的语境只会更乱）。返回是否至少送出一条。
pub async fn dispatch(pusher: &ChannelsPusher, cand: &IntentCandidate, texts: &[String]) -> bool {
    let mut sent_any = false;
    for (index, text) in texts.iter().enumerate() {
        let delay = crate::channels::pacing_delay(index);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        match pusher
            .push(cand.channel.clone(), cand.external_id.clone(), text.clone())
            .await
        {
            Ok(()) => {
                sent_any = true;
                tracing::info!(
                    channel = %cand.channel,
                    user = %cand.external_id,
                    index,
                    "消息发送成功"
                );
            }
            Err(e) => {
                tracing::warn!(
                    channel = %cand.channel,
                    user = %cand.external_id,
                    index,
                    error = %e,
                    "消息未发送，停止后续派发"
                );
                break;
            }
        }
    }
    sent_any
}
