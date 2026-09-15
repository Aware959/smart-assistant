//! 记忆提取：判断用户消息是否值得沉淀为记忆，并产出事实化内容。
//!
//! 该模块自包含 LLM 判定所需的 prompt、调用与结果解析，
//! 对外只暴露 [`MemoryExtraction`] 一种类型。

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Result, SqlError};
use crate::llm::chat::{self, ChatMessage};
use crate::world::relation::RelationEvent;

/// LLM 对单条用户消息的记忆判定结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryExtraction {
    /// 是否把这条用户消息沉淀为记忆（事实）。
    #[serde(default)]
    pub is_memory: bool,
    /// 值得记住时给出的事实化内容（如无则由原文替代）。
    #[serde(default, alias = "memory_summary")]
    pub memory_content: Option<String>,
    /// 事实类型，默认 fact。
    #[serde(default = "default_memory_type")]
    pub memory_type: String,
    /// 记忆层级：short（短期/易过期）| intent（意向/计划）| core（长期强事实），默认 core。
    #[serde(default = "default_tier")]
    pub tier: String,
    /// 本条消息对两人关系的影响事件标签
    /// （neutral / warm / ambig / conflict / reconcile），默认 neutral。
    /// 与记忆判定共用同一次 LLM 调用，供世界引擎按事件调整关系。
    #[serde(default = "default_relation")]
    pub relation: String,
}

fn default_memory_type() -> String {
    "fact".to_string()
}

fn default_tier() -> String {
    "core".to_string()
}

fn default_relation() -> String {
    "neutral".to_string()
}

impl Default for MemoryExtraction {
    fn default() -> Self {
        Self {
            is_memory: false,
            memory_content: None,
            memory_type: default_memory_type(),
            tier: default_tier(),
            relation: default_relation(),
        }
    }
}

impl MemoryExtraction {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 取出可落库的事实文本：优先用 LLM 事实化内容，否则回退原文。
    pub fn content_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.memory_content
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or(fallback)
    }

    /// 本条消息对应的关系事件（未知标签回退到 Neutral）。
    pub fn relation_event(&self) -> RelationEvent {
        RelationEvent::from_label(&self.relation)
    }
}

/// 一次 LLM 调用：判断是否沉淀记忆并产出事实化内容。
///
/// 配置了 `LLM_EXTRACT_MODEL` 时用该模型，否则复用对话模型
/// （记忆判定/事实化质量要求高，推荐与对话模型分离）。
pub fn extract_from_text(user_input: &str) -> Result<MemoryExtraction> {
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: MEMORY_EXTRACTION_PROMPT.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user_input.to_string(),
        },
    ];

    let raw = if let Some(model) = extract_model() {
        chat::complete_structured(&messages, &model, "memory_extraction", extraction_schema())?
    } else {
        chat::complete_structured(&messages, &Config::get().llm_model, "memory_extraction", extraction_schema())?
    };

    parse_raw(&raw)
}

/// `MemoryExtraction` 的 JSON Schema：交给支持 `json_schema` 的端点（llama.cpp 系）
/// 用 grammar 约束输出，强制纯 JSON、不再混入思考段。
fn extraction_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "is_memory": { "type": "boolean" },
            "memory_content": { "type": ["string", "null"] },
            "memory_type": { "type": "string" },
            "tier": { "type": "string" },
            "relation": { "type": "string" }
        },
        "required": ["is_memory", "memory_content", "memory_type", "tier", "relation"]
    })
}

fn extract_model() -> Option<String> {
    let m = Config::get().llm_extract_model.trim();
    (!m.is_empty()).then(|| m.to_string())
}

#[derive(Debug, Deserialize)]
struct ExtractionEnvelope {
    #[serde(default)]
    is_memory: bool,
    #[serde(default, alias = "memory_summary")]
    memory_content: Option<String>,
    #[serde(default = "default_memory_type")]
    memory_type: String,
    #[serde(default = "default_tier")]
    tier: String,
    #[serde(default = "default_relation")]
    relation: String,
}

/// 从 LLM 原始输出解析结构化结果，容忍外层 markdown 代码围栏与多余文本。
pub fn parse_raw(raw: &str) -> Result<MemoryExtraction> {
    let cleaned = strip_code_fences(raw);
    let json_start = cleaned
        .find('{')
        .ok_or_else(|| SqlError::Config("LLM 输出不包含 JSON 对象".to_string()))?;
    let json_end = cleaned
        .rfind('}')
        .ok_or_else(|| SqlError::Config("LLM 输出不包含闭合 JSON 对象".to_string()))?;
    let json_str = &cleaned[json_start..=json_end];

    let parsed: ExtractionEnvelope = serde_json::from_str(json_str).map_err(|e| {
        SqlError::Config(format!("解析提取结果失败: {e}; 原始输出: {}", raw))
    })?;

    Ok(MemoryExtraction {
        is_memory: parsed.is_memory,
        memory_content: parsed.memory_content,
        memory_type: parsed.memory_type,
        tier: parsed.tier,
        relation: parsed.relation,
    })
}

fn strip_code_fences(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut result = trimmed.to_string();
    if result.starts_with("```") {
        if let Some(end) = result.rfind("```") {
            result = result[3..end].trim().to_string();
        }
    }
    result
}

/// 记忆判定系统提示词（英文直接给 LLM，中文注释供开发者阅读）。
///
/// 本判定在一次调用里同时产出两件事：是否值得沉淀记忆，以及本条消息对关系
/// 的影响事件。输出为严格 JSON：is_memory / memory_content / memory_type /
/// tier / relation，不允许多余文本。
///
/// is_memory 为 true 仅限实质信息（偏好/计划/经历），闲聊一律 false；
/// memory_content 归一成第三人称一句话（禁止"我/我们"，用"对方"）；
/// tier 为 short（临时易失效）/ intent（计划/进行中）/ core（长期事实与偏好）；
/// relation 为 neutral（日常闲聊）/ warm（关心/惦记）/ ambig（暧昧/亲密）/
/// conflict（矛盾/吵架/失望/误会）/ reconcile（和解/道歉），只按本条消息的
/// 语气判定，无情绪的普通对话必须 neutral；不虚构用户输入中不存在的信息。
const MEMORY_EXTRACTION_PROMPT: &str = r#"
You are a conversational-memory analysis engine. Decide whether this user message is worth persisting as a memory, and if so produce one normalized factual statement with a tier. Also classify the ONE relation event this message produces.

Output must be strict JSON — no extra text, no markdown fences, no comments. Shape:
{
  "is_memory": true,
  "memory_content": "one normalized factual sentence",
  "memory_type": "fact",
  "tier": "core",
  "relation": "neutral"
}

tier rules:
- "short": temporary, short-lived state that will pass soon (e.g. "我今天身体不舒服", "在赶一个项目", "感冒还没好").
- "intent": an ongoing plan/intention, fairly stable but subject to change (e.g. "打算下个月去看电影", "下周要考试", "准备搬家").
- "core": long-term stable facts and preferences (e.g. "对方最喜欢的颜色是蓝色", "养了一只叫豆豆的猫").

relation rules — judge by the tone of this single message:
- "neutral": ordinary small talk, plain questions, routine chat; no emotional charge. When unsure, default to neutral.
- "warm": caring, thinking of the other, heartwarming, gratitude (e.g. "记得按时吃饭", "你辛苦了我要对你好一点", "谢谢你一直陪着我").
- "ambig": flirtatious, testing, romantic or intimate undercurrent (e.g. "有点想你了", "你觉得我这个人怎么样", 打情骂俏).
- "conflict": disagreement, argument, complaint, disappointment, blame, sulking, misunderstanding (e.g. "你总是这样敷衍我", "我们是不是不合适", "你根本不懂我").
- "reconcile": apology, making up, extending an olive branch, clearing a misunderstanding (e.g. "刚才是我不对", "对不起,我不该那样说", "别生气了嘛").

Rules:
1. is_memory: TRUE only when the message carries substantive info worth recalling later (personal info, preferences, important experiences, plans, task progress). Always FALSE for casual small talk, plain questions, empty greetings, repetitive complaints with nothing new, or pure venting without concrete facts. Rather skip than over-store.
2. memory_content: when is_memory is true, give ONE normalized sentence — strip colloquial filler, keep a single core fact, no long excerpts. Person must be normalized: no matter how the user refers to themselves, always use "对方" (the other person) as subject; never use first-person pronouns ("我/我们/俺/咱们/本人") except inside a verbatim quote. E.g. user says "我最喜欢蓝色了" → "对方最喜欢的颜色是蓝色".
3. memory_type common values: fact / preference / personal / todo / event; default to fact when unsure.
4. relation is independent of is_memory: a message can be both memory-worthy and relation-relevant, or only one, or neither. Pick the single most salient event of THIS message.
5. Never invent information that is not in the user input.

(Compatibility: "memory_content" may also be written as "memory_summary".)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_raw_reads_relation_label() {
        let raw = r#"{"is_memory": true, "memory_content": "对方最近很忙", "memory_type": "fact", "tier": "short", "relation": "warm"}"#;
        let parsed = parse_raw(raw).unwrap();
        assert_eq!(parsed.relation, "warm");
        assert_eq!(parsed.relation_event(), crate::world::relation::RelationEvent::Warm);
    }

    #[test]
    fn parse_raw_defaults_to_neutral_when_missing() {
        // 兼容旧格式输出：没有 relation 字段时按 neutral 处理。
        let raw = r#"{"is_memory": false, "memory_content": null, "memory_type": "fact", "tier": "core"}"#;
        let parsed = parse_raw(raw).unwrap();
        assert_eq!(parsed.relation, "neutral");
        assert_eq!(
            parsed.relation_event(),
            crate::world::relation::RelationEvent::Neutral
        );
    }

    #[test]
    fn unknown_relation_label_falls_back_to_neutral() {
        let raw = r#"{"is_memory": false, "memory_content": null, "memory_type": "fact", "tier": "core", "relation": "???whatever"}"#;
        let parsed = parse_raw(raw).unwrap();
        assert_eq!(
            parsed.relation_event(),
            crate::world::relation::RelationEvent::Neutral
        );
    }
}
