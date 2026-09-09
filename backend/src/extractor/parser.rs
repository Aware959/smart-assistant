use serde::{Deserialize, Serialize};

use crate::llm::entity::ENTITY_EXTRACTION_SYSTEM_PROMPT;
use crate::llm::chat::{self, ChatMessage};
use crate::error::{Result, SqlError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    #[serde(alias = "type")]
    pub entity_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedRelation {
    #[serde(alias = "from")]
    pub source: String,
    #[serde(alias = "to")]
    pub target: String,
    #[serde(alias = "type")]
    pub relation: String,
    #[serde(default = "default_weight")]
    pub weight: f32,
}

fn default_weight() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    /// 是否把这条用户消息沉淀为记忆（事实）。
    #[serde(default)]
    pub is_memory: bool,
    /// 值得记住时给出的事实化内容（如无则由原文替代）。
    #[serde(default, alias = "memory_summary")]
    pub memory_content: Option<String>,
    /// 事实类型，默认 fact。
    #[serde(default = "default_memory_type")]
    pub memory_type: String,
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

fn default_memory_type() -> String {
    "fact".to_string()
}

impl Default for ExtractionResult {
    fn default() -> Self {
        Self {
            is_memory: false,
            memory_content: None,
            memory_type: default_memory_type(),
            entities: Vec::new(),
            relations: Vec::new(),
        }
    }
}

/// 一次性提取：让 LLM 决定是否沉淀记忆、产出事实化内容，并提取实体与关系。
pub fn extract_from_text(user_input: &str) -> Result<ExtractionResult> {
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: ENTITY_EXTRACTION_SYSTEM_PROMPT.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user_input.to_string(),
        },
    ];

    let raw = chat::complete(&messages)?;

    parse_raw(&raw)
}

impl ExtractionResult {
    pub fn empty() -> Self {
        Self {
            is_memory: false,
            memory_content: None,
            memory_type: default_memory_type(),
            entities: Vec::new(),
            relations: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ExtractionEnvelope {
    #[serde(default)]
    is_memory: bool,
    #[serde(default, alias = "memory_summary")]
    memory_content: Option<String>,
    #[serde(default = "default_memory_type")]
    memory_type: String,
    #[serde(default)]
    entities: Vec<ExtractedEntity>,
    #[serde(default)]
    relations: Vec<ExtractedRelation>,
}

/// 从 LLM 原始输出解析结构化结果，容忍外层 markdown 代码围栏与多余文本。
pub fn parse_raw(raw: &str) -> Result<ExtractionResult> {
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

    Ok(ExtractionResult {
        is_memory: parsed.is_memory,
        memory_content: parsed.memory_content,
        memory_type: parsed.memory_type,
        entities: parsed.entities,
        relations: parsed.relations,
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
