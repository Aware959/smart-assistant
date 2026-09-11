//! 记忆提取：判断用户消息是否值得沉淀为记忆，并产出事实化内容。
//!
//! 该模块自包含 LLM 判定所需的 prompt、调用与结果解析，
//! 对外只暴露 [`MemoryExtraction`] 一种类型。

use serde::{Deserialize, Serialize};

use crate::config::Config;
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
    /// 记忆层级：short（短期/易过期）| intent（意向/计划）| core（长期强事实），默认 core。
    #[serde(default = "default_tier")]
    pub tier: String,
}

fn default_memory_type() -> String {
    "fact".to_string()
}

fn default_tier() -> String {
    "core".to_string()
}

impl Default for MemoryExtraction {
    fn default() -> Self {
        Self {
            is_memory: false,
            memory_content: None,
            memory_type: default_memory_type(),
            tier: default_tier(),
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
            "tier": { "type": "string" }
        },
        "required": ["is_memory", "memory_type", "tier"]
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
你是一个对话记忆分析引擎。判断这条用户消息是否值得沉淀为记忆，并给出事实化内容与层级。

输出必须是严格的 JSON，不要包含任何多余文本、markdown 代码块或注释。格式如下：
{
  "is_memory": true,
  "memory_content": "事实化的一句话陈述",
  "memory_type": "fact",
  "tier": "core"
}

tier 取值与判定规则：
- "short"：临时、短期状态，会较快失效（如"我今天身体不舒服"、"在赶一个项目"、"感冒还没好"）。
- "intent"：正在计划/打算/进行中的事，较稳定但会变（如"打算下个月去看电影"、"下周要考试"、"准备搬家"）。
- "core"：长期稳定的事实与偏好（如"他说爸妈从小没爱过他"、"最喜欢的颜色是蓝色"、"养了一只叫豆豆的猫"）。

规则：
1. is_memory：仅当消息包含值得日后回想的实质信息时才为 true（个人信息、偏好、重要经历、计划、任务进度等）。闲聊寒暄、单纯提问、昵称寒暄、无新信息的重复抱怨、情绪宣泄而无具体事实时一律 false，宁可漏掉不要硬存。
2. memory_content：is_memory 为 true 时给出规范化的一句话陈述——去除口语、只保留一个核心事实，不要写成大段摘抄。人称必须归一：无论用户原句怎么自称，一律用"对方"（或"用户"）作主语，禁止出现"我/我们/俺/咱们/本人"等第一人称（原话引语除外）。例如用户说"我最喜欢蓝色了"应写成"对方最喜欢的颜色是蓝色"。
3. memory_type 常用取值：fact / preference / personal / todo / event，不确定用 fact。
4. 不要虚构用户输入中不存在的信息。

（兼容写法："memory_content" 也可写作 "memory_summary"。）
"#;
