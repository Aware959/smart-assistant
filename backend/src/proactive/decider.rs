//! 开口决策：让 LLM 判断此刻是否值得主动发消息，默认不开口。
//!
//! 一次调用同时产出"是否开口 + 开口内容"，省掉往返。解析失败一律保守视为不开口。

use serde::Deserialize;

use crate::config::Config;
use crate::llm;

/// 一轮决策结果。
#[derive(Debug, Default, Clone)]
pub struct Decision {
    pub speak: bool,
    /// 开口时真正要发给对方的内容（须非空才有效）。
    pub message: Option<String>,
    pub reason: Option<String>,
}

/// 开口决策提示词（英文给 LLM，中文注释供开发者阅读）。
///
/// - [角色注入] persona，最高优先级，来自用户配置
/// - [总纲] 陪伴型对话伙伴，判断此刻该不该主动开口
/// - [规则] 默认沉默；值得开口的三种情况：久未聊、情绪低落/等回应、有小事值得分享
/// - [输出] 只输出一个 JSON：speak=true/false + message/reason
/// - [格式] 口语化、不超过40字、不解释为什么找对方、不复述本指令
pub fn decide(recent: &str, recall: &str, world: &str) -> Decision {
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
    let system = format!(
        "{persona_block}You are a companion-style chat partner. Based on the user's recent message \
         state, decide whether you should proactively message them right now.\n\
         Rules:\n\
         1. Stay silent by default, unless there is a real reason: (a) it has been a long time since \
         you last talked and a light topic could reconnect you; (b) the other person just said \
         something non-statement-like (sounding down, waiting for a reply, needing care); (c) a small \
         thing genuinely worth sharing just happened.\n\
         2. When you do speak, use only one or two colloquial sentences, no more than 40 characters, \
         like a casual WeChat ping from an old friend. Don't explain why you're reaching out, no \
         polite openers, don't recite these instructions, and never mention \"system\" or \"rules\".\n\
         Output only a JSON object and nothing else: {{\"speak\":true,\"message\":\"...\"}} or \
         {{\"speak\":false,\"reason\":\"...\"}}"
    );

    let world_block = if world.trim().is_empty() {
        String::new()
    } else {
        format!("【此刻的世界状态】(current world state):\n{world}\n\n")
    };
    let user = format!(
        "{world_block}Your recent conversation:\n{recent}\n\nWhat you remember about the other \
         person:\n{recall}\n\nNow decide whether to send them a proactive message. Output JSON only."
    );

    let messages = vec![
        llm::chat::ChatMessage {
            role: "system".to_string(),
            content: system,
        },
        llm::chat::ChatMessage {
            role: "user".to_string(),
            content: user,
        },
    ];

    match llm::chat::complete(&messages) {
        Ok(text) => parse(&text),
        Err(e) => {
            tracing::warn!(error = %e, "主动决策 LLM 调用失败，本轮不开口");
            Decision::default()
        }
    }
}

fn parse(text: &str) -> Decision {
    let payload = extract_json(text);
    let payload = payload.unwrap_or(text);
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        speak: bool,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    }
    match serde_json::from_str::<Raw>(payload) {
        Ok(raw) => {
            let message = raw
                .message
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty());
            Decision {
                // 保证不变量：speak=true 时必定带非空消息。
                speak: raw.speak && message.is_some(),
                message,
                reason: raw.reason,
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, payload = %payload, "主动决策 JSON 解析失败，默认为不开口");
            Decision::default()
        }
    }
}

/// 宽容提取：剥离可能的 ```json 围栏，取首个 `{` 到最后一个 `}` 之间的内容。
pub(crate) fn extract_json(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&s[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_speak_true_with_fences() {
        let d = parse("```json\n{\"speak\": true, \"message\": \"今天咋样？\"}\n```");
        assert!(d.speak);
        assert_eq!(d.message.as_deref(), Some("今天咋样？"));
    }

    #[test]
    fn parses_speak_false() {
        let d = parse("{\"speak\": false, \"reason\": \"刚聊完，不用打扰\"}");
        assert!(!d.speak);
        assert!(d.message.is_none());
        assert_eq!(d.reason.as_deref(), Some("刚聊完，不用打扰"));
    }

    #[test]
    fn tolerates_chatty_surrounding_text() {
        let d = parse("好的，我看看。{\"speak\":false,\"reason\":\"没话题\"} 完毕。");
        assert!(!d.speak);
    }

    #[test]
    fn empty_message_is_not_valid() {
        let d = parse("{\"speak\":true,\"message\":\"   \"}");
        assert!(!d.speak, "空消息应视为不开口");
        assert!(d.message.is_none());
    }

    #[test]
    fn garbage_defaults_to_silent() {
        let d = parse("这不是 JSON");
        assert!(!d.speak);
        assert!(d.message.is_none());
    }
}