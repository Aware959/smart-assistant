//! 记忆提取：判断用户消息是否值得沉淀为记忆，并产出事实化内容。
//!
//! 该模块自包含 LLM 判定所需的 prompt、调用与结果解析，
//! 对外只暴露 [`MemoryExtraction`] 一种类型。

use serde::{Deserialize, Serialize};

use crate::error::{Result, SqlError};
use crate::llm::chat::{self, ChatMessage};

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
}

fn default_memory_type() -> String {
    "fact".to_string()
}

impl Default for MemoryExtraction {
    fn default() -> Self {
        Self {
            is_memory: false,
            memory_content: None,
            memory_type: default_memory_type(),
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
}

/// 一次 LLM 调用：判断是否沉淀记忆并产出事实化内容。
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

    let raw = chat::complete(&messages)?;

    parse_raw(&raw)
}

#[derive(Debug, Deserialize)]
struct ExtractionEnvelope {
    #[serde(default)]
    is_memory: bool,
    #[serde(default, alias = "memory_summary")]
    memory_content: Option<String>,
    #[serde(default = "default_memory_type")]
    memory_type: String,
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

/// 记忆判定的系统提示词。
const MEMORY_EXTRACTION_PROMPT: &str = r#"
你是一个对话分析引擎。判断这条用户消息是否值得沉淀为记忆（事实）。

输出必须是严格的 JSON，不要包含任何多余文本、markdown 代码块或注释。格式如下：
{
  "is_memory": true,
  "memory_content": "事实化的一句话陈述",
  "memory_type": "fact"
}

规则：
1. is_memory：仅当消息包含需要长期记住的实质信息时才为 true（如个人信息、偏好、事实、任务进度）；闲聊寒暄、单纯提问、无新信息时一律 false。
2. memory_content：is_memory 为 true 时给出规范化的事实陈述（去除口语、补全指代）；为 false 时可省略或为空字符串。
3. memory_type 常用取值：fact / preference / personal / todo / event，不确定用 fact。
4. 不要虚构用户输入中不存在的信息。

（兼容写法："memory_content" 也可写作 "memory_summary"。）
"#;
