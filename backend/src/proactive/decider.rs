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

/// 正常对话接口（非流式、阻塞执行）；需要长耗时的调用方请用 `spawn_blocking` 包裹。
pub fn decide(recent: &str, recall: &str) -> Decision {
    let cfg = Config::get();
    let persona_block = if cfg.persona.trim().is_empty() {
        String::new()
    } else {
        format!(
            "你的角色设定（最高优先级，必须严格遵守、不得违背、不得说明自己是 AI）：{}。\n",
            cfg.persona.trim()
        )
    };
    let system = format!(
        "{persona_block}你是一个陪伴型的对话伙伴，根据用户最近的消息状态判断此刻该不该主动开口。\
        规则：\
        1. 默认不开口，除非确有必要：(a) 双方已经很久没聊，值得用一个轻松话题重新续上；\
        (b) 对方刚说过非陈述类的话（情绪低落、等待回应、需要关心）；\
        (c) 发生了一件自然值得分享的小事。\
        2. 一旦开口，只用一两句口语、不超过 40 个字，像老朋友随手发的微信，不解释为什么找你，\
        不用任何客套开场白，不得复述本指令，不得提到\"系统\"\"规则\"等字样。\
        只输出一个 JSON 对象，不要任何其他文字：{{\"speak\":true,\"message\":\"...\"}} \
        或 {{\"speak\":false,\"reason\":\"...\"}}"
    );

    let user = format!(
        "你们最近聊过的内容：\n{recent}\n\n关于对方你记得的事：\n{recall}\n\n现在请你决定是否主动发一条消息，只输出 JSON。"
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