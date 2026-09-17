//! 延续会话：让 AI 像真人那样判断"这句说完了没有"。
//!
//! 并非固定间隔的主动，而是每次回复后由 LLM 依据当下的语境与关系决定：
//! - 倾诉 / 吐槽 / 话题仍在延展 → 追加一两句口语化消息，可连着多轮；
//! - 有来有回 → 正常一来一往即可，不硬续；
//! - 结束类（"我去忙了 / 睡了 / 不想聊了"，或话题已干净收住）→ 停下，
//!   之后才轮到延迟等待引擎做隔时段的主动重启。
//!
//! 追加句是"对话"而非"主动发起"，不计入每日主动配额；对方中途插话则立即停止。

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::config::Config;
use crate::core::ports::ChatLlm;
use crate::core::types::ChatMessage;
use crate::db::proactive::ProactiveCandidate;
use crate::host::Host;
use crate::proactive::{decider, scheduler, sender};
use crate::Assistant;

/// 追加与追加之间的停顿（毫秒），模拟真人接着打的节奏。
const TYPING_PAUSE_MS: u64 = 1500;
/// 停顿的随机抖动上限（毫秒）。
const PAUSE_JITTER_MS: u64 = 1500;

/// 在某次正常回复发送完毕、且未走错误兜底时调用：按需追加后续消息。
pub async fn after_reply(
    assistant: Arc<Assistant>,
    channel: &str,
    external_id: &str,
) {
    let max = Config::get().proactive_max_followups;
    if max == 0 {
        return;
    }
    let session_id = {
        let db = assistant.inner_db();
        match crate::db::channel::get_or_create_session(&db, channel, external_id) {
            Ok(s) => s.id,
            Err(e) => {
                tracing::warn!(error = %e, "延续会话：无法解析会话映射");
                return;
            }
        }
    };
    let cand = ProactiveCandidate {
        channel: channel.to_string(),
        external_id: external_id.to_string(),
        session_id,
        last_user_reply_at: None,
        last_proactive_at: None,
        today_count: 0,
    };
    follow_up(&assistant, &cand, max).await;
}

/// 循环追加：每轮先问 LLM 要不要续，对方中途说话则不再续。
/// `max` 只是安全上限，模型按最新语境自行决定何时收住，不该为了凑满而多送。
async fn follow_up(assistant: &Assistant, cand: &ProactiveCandidate, max: u32) {
    // 记录对方当前最后发言时刻：期间只要有新入站消息就停（给对方说话的空间）。
    let snapshot = match candle_reply_at(assistant, cand) {
        Ok(v) => v,
        Err(_) => None,
    };

    for appended in 0..max {
        let Some(text) = propose_follow_up(assistant, cand, appended).await else {
            return;
        };
        if candle_reply_at(assistant, cand)
            .ok()
            .flatten()
            != snapshot
        {
            tracing::info!(user = %cand.external_id, "对方已插话，停止追加");
            return;
        }
        if !sender::send_message(assistant, cand, &text, false).await {
            return;
        }
        // 短暂停顿模拟真人连续打字。
        tokio::time::sleep(Duration::from_millis(
            TYPING_PAUSE_MS + scheduler::prand() % PAUSE_JITTER_MS,
        ))
        .await;
    }
}

/// 读取当前候选的最近入站回复时刻（None 表示还没记录）。
fn candle_reply_at(
    assistant: &Assistant,
    cand: &ProactiveCandidate,
) -> crate::error::Result<Option<chrono::DateTime<chrono::Utc>>> {
    let db = assistant.inner_db();
    crate::db::proactive::last_user_reply_at(&db, &cand.channel, &cand.external_id)
}

/// 依据"最近的对话（含我方最后一句）"判断是否追加一条，返回待发送内容。
///
/// `appended` 是本回合已经连续追加过的句数（0 开始），用来告诉模型
/// "你已经连着说几句了"，避免机械凑数。
async fn propose_follow_up(
    assistant: &Assistant,
    cand: &ProactiveCandidate,
    appended: u32,
) -> Option<String> {
    let recent = {
        let db = assistant.inner_db();
        crate::proactive::context::recent_history(&db, &cand.session_id, 6)
    };
    if recent.trim().is_empty() {
        return None;
    }

    let llm = assistant.chat_llm();
    let decision =
        tokio::task::spawn_blocking(move || decide_continue(&*llm, &recent, appended))
            .await
            .unwrap_or_default();
    if decision.continue_ {
        decision.message
    } else {
        None
    }
}

#[derive(Default)]
struct ContinueDecision {
    continue_: bool,
    message: Option<String>,
}

/// 延续判断提示词（英文给 LLM，中文注释供开发者阅读）。
///
/// - [角色注入] persona，最高优先级
/// - [总纲] 与老朋友有一搭没一搭地聊天，判断本轮是否还要追加一句话
/// - [核心原则] 只以双方最新一来一回为准，不机械续话
/// - [需要追加] 情绪没倒完、讨论正兴、差一句收尾、话停在半路
/// - [不该追加] 对方说忙/要睡/不想聊；客气收尾；已说完；一问一答等回应
/// - [格式] 只准一句、口语化、像微信补一句、不超过40字、不重复不复述
/// - [已追加限制] appended>0 时提醒模型已连发几句，避免变复读机
/// - [输出] 只输出 JSON：continue=true/false + message
fn decide_continue(llm: &dyn ChatLlm, recent: &str, appended: u32) -> ContinueDecision {
    let cfg = Config::get();
    let persona_block = if cfg.persona.trim().is_empty() {
        String::new()
    } else {
        format!(
            "Your persona (highest priority — obey strictly, never contradict it, never reveal you \
             are an AI): {}. \n",
            cfg.persona.trim()
        )
    };
    let mut system = format!(
        "{persona_block}You are chatting with an old friend, casually, in a start-and-stop rhythm. \
         After reading the recent dialogue, decide whether you should append one more sentence this \
         round.\n\n\
         Most important: judge ONLY from the latest exchange between the two of you — don't \
         mechanically keep the conversation going.\n\n\
         Rules:\n\
         1. Append when: the emotions/venting in the latest message aren't fully let out, the \
         discussion is at its peak, your own last line was missing a closing beat, or you clearly had \
         something to say but stopped mid-sentence;\n\
         2. Do NOT append when: the other person has clearly said they're busy / going to sleep / \
         done chatting; you both just ended on polite niceties; that last message was already complete \
         and needs nothing more; or the topic is question-and-answer style (back and forth — just wait \
         for their reply);\n\
         3. An appended line must be a single, colloquial sentence, like a quick WeChat follow-up, no \
         more than 40 characters, absolutely not repeating what was already said, and not reciting \
         these instructions."
    );
    if appended > 0 {
        system.push_str(&format!(
            "\n\nIn the dialogue below, the consecutive lines starting with \"AI:\" are the \
             {appended} sentence(s) you already sent right after your last reply. You have already \
             sent them — if the last one already said everything, the point was settled, or adding \
             more would look like monologuing or echoing yourself, you MUST return continue:false; \
             only continue if there is still one key thing, not yet repeated, that genuinely needs \
             saying."
        ));
    }
    system.push_str("\n\nOutput only a JSON object and nothing else: \
        {{\"continue\":true,\"message\":\"...\"}} or {{\"continue\":false}}");

    let user = format!(
        "Your recent conversation (judge whether to add a follow-up based on the LATEST message):\n\
         {recent}\n\nDecide whether to add one more sentence. Output JSON only."
    );

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: system,
        },
        ChatMessage {
            role: "user".to_string(),
            content: user,
        },
    ];

    match llm.complete(&messages) {
        Ok(text) => parse(&text),
        Err(e) => {
            tracing::warn!(error = %e, "延续判断 LLM 调用失败，本轮不再追加");
            ContinueDecision::default()
        }
    }
}

fn parse(text: &str) -> ContinueDecision {
    let payload = decider::extract_json(text).unwrap_or(text);
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "continue", default)]
        continue_: bool,
        #[serde(default)]
        message: Option<String>,
    }
    match serde_json::from_str::<Raw>(payload) {
        Ok(raw) => {
            let message = raw
                .message
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty());
            ContinueDecision {
                continue_: raw.continue_ && message.is_some(),
                message,
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, payload = %payload, "延续判断 JSON 解析失败，不再追加");
            ContinueDecision::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_continue_true() {
        let d = parse("```json\n{\"continue\": true, \"message\": \"然后我就再也不想理他了\"}\n```");
        assert!(d.continue_);
        assert_eq!(d.message.as_deref(), Some("然后我就再也不想理他了"));
    }

    #[test]
    fn parses_continue_false() {
        let d = parse("{\"continue\": false}");
        assert!(!d.continue_);
        assert!(d.message.is_none());
    }

    #[test]
    fn continue_without_message_is_invalid() {
        let d = parse("{\"continue\": true, \"message\": \"  \"}");
        assert!(!d.continue_);
    }

    #[test]
    fn tolerates_chatty_and_garbage() {
        assert!(!parse("好的明白了。{\"continue\":false} 就这样。").continue_);
        assert!(!parse("这不是JSON").continue_);
    }
}