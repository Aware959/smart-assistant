//! 发送派发：按候选通道选择实现，发送成功后回写状态与历史。

use std::sync::atomic::Ordering;

use crate::channels::shared_push;
use crate::db::proactive::ProactiveCandidate;
use crate::Assistant;

/// 通道当前是否可能把消息送出去（telegram 已连接 / iLink 仍有新鲜 token）。
/// 用于在触发 LLM 决策之前先剔除不可推的用户，省下无谓的模型调用。
pub fn pushable(cand: &ProactiveCandidate) -> bool {
    match cand.channel.as_str() {
        "telegram" => {
            if crate::config::Config::get().telegram_bot_token.trim().is_empty() {
                return false;
            }
            shared_push().telegram_ready.load(Ordering::Relaxed)
        }
        "ilink" => ilink_has_fresh_token(&cand.external_id),
        other => {
            tracing::warn!(channel = other, "未知通道，无法主动推送");
            false
        }
    }
}

/// 主动推送（由延迟等待引擎触发）：发送并计入每日主动配额与历史。
/// 等价于 [`send_message`] 的 `count_as_proactive = true` 版本。
pub async fn dispatch(assistant: &Assistant, cand: &ProactiveCandidate, text: &str) -> bool {
    send_message(assistant, cand, text, true).await
}

/// 发送一条消息并回写历史；`count_as_proactive` 为 true 时同时计入每日主动配额。
///
/// - 延续会话中的追加句是"对话"，不计入主动配额；
/// - 延迟等待引擎的主动发起才计入配额。
pub async fn send_message(
    assistant: &Assistant,
    cand: &ProactiveCandidate,
    text: &str,
    count_as_proactive: bool,
) -> bool {
    let result = match cand.channel.as_str() {
        "telegram" => send_telegram(cand, text).await,
        "ilink" => crate::channels::ilink::push_send(&cand.external_id, text).await,
        other => {
            tracing::warn!(channel = other, "暂不支持主动推送的通道");
            return false;
        }
    };

    match result {
        Ok(()) => {
            record_sent(assistant, cand, text, count_as_proactive);
            tracing::info!(
                channel = %cand.channel,
                user = %cand.external_id,
                proactive = %count_as_proactive,
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

async fn send_telegram(cand: &ProactiveCandidate, text: &str) -> Result<(), String> {
    let chat_id = cand
        .external_id
        .parse::<i64>()
        .map_err(|_| "telegram 外部 id 非数字".to_string())?;
    crate::channels::telegram::push_send(chat_id, text).await
}

fn ilink_has_fresh_token(external_id: &str) -> bool {
    let cfg = crate::config::Config::get();
    let reg = shared_push().ilink.lock().unwrap_or_else(|p| p.into_inner());
    reg.as_ref()
        .and_then(|r| r.contacts.get(external_id))
        .map(|c| c.is_fresh(chrono::Utc::now(), cfg.ilink_context_max_age_hours))
        .unwrap_or(false)
}

/// 成功后把主动消息写进该用户的历史（避免上下文断裂），并按需刷新主动状态。
fn record_sent(
    assistant: &Assistant,
    cand: &ProactiveCandidate,
    text: &str,
    count_as_proactive: bool,
) {
    let db = assistant.inner_db();
    if let Err(e) = crate::db::message::create(&db, &cand.session_id, "assistant", text) {
        tracing::warn!(error = %e, "消息写回历史失败");
    }
    if count_as_proactive {
        if let Err(e) = crate::db::proactive::record_proactive_send(
            &db,
            &cand.channel,
            &cand.external_id,
            &cand.session_id,
        ) {
            tracing::warn!(error = %e, "主动状态记录失败");
        }
    }
}