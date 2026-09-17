//! 记忆提取：判断用户消息是否值得沉淀为记忆，并产出事实化内容。
//!
//! 分两次调用，各自只做一件事：
//! 1. **判定**：只出窄标签（is_memory / memory_type / tier / relation），
//!    输出结构被枚举收窄，模型几乎没有写歪的余地；
//! 2. **事实化**：仅在判定为"值得记住"时，把原话归一成一条第三人称事实句，
//!    输出是纯文本而非 JSON。
//!
//! 该模块自包含两次调用所需的 prompt、调用与结果解析，
//! 对外只暴露 [`MemoryExtraction`] 一种类型；模型输出一律当作**不可信输入**宽容解析。

use crate::config::Config;
use crate::error::{Result, SqlError};
use crate::llm::chat::{self, ChatMessage};
use crate::world::relation::RelationEvent;

/// LLM 对单条用户消息的记忆判定结果（判定标签 + 事实化内容）。
#[derive(Debug, Clone)]
pub struct MemoryExtraction {
    /// 是否把这条用户消息沉淀为记忆（事实）。
    pub is_memory: bool,
    /// 值得记住时给出的事实化内容（如无则由原文替代）。
    pub memory_content: Option<String>,
    /// 事实类型，默认 fact。
    pub memory_type: String,
    /// 记忆层级：short（短期/易过期）| intent（意向/计划）| core（长期强事实），默认 core。
    pub tier: String,
    /// 本条消息对两人关系的影响事件标签
    /// （neutral / warm / ambig / conflict / reconcile），默认 neutral。
    /// 与记忆判定共用同一次 LLM 调用，供世界引擎按事件调整关系。
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

impl MemoryExtraction {
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

/// 记忆提取：先判定标签，判定为"值得记住"时才追加一次事实化调用。
///
/// **为什么拆成两次调用**：判定本质是*分类*，事实化才是*生成*。混在一次调用里时，
/// 模型会为了满足"生成一段话"的要求而在标签字段上偷懒（写 `null`、写错类型），
/// 而这些标签下游要拿去做硬分支（是否落库、TTL 长短、关系演化），脏数据代价不对称。
/// 拆开后：判定调用输出只有 4 个枚举值，schema 还能对枚举做强约束；事实化输出
/// 纯文本，连 JSON 转义这个失败点都没有了。
///
/// 多数轮次（闲聊、提问）判定为 false，**不会**产生第二次调用，反而更快。
///
/// 配置了 `LLM_EXTRACT_MODEL` 时用该模型，否则复用对话模型
/// （记忆判定/事实化质量要求高，推荐与对话模型分离）。
pub fn extract_from_text(user_input: &str) -> Result<MemoryExtraction> {
    let Judgement {
        is_memory,
        memory_type,
        tier,
        relation,
    } = judge(user_input)?;

    // 事实化失败不算失败：上层会用原文兜底（`MemoryExtraction::content_or`）。
    let memory_content = if is_memory { summarize(user_input) } else { None };

    Ok(MemoryExtraction {
        is_memory,
        memory_content,
        memory_type,
        tier,
        relation,
    })
}

/// 第一步：只判定标签，不生成任何自然语言。
fn judge(user_input: &str) -> Result<Judgement> {
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: MEMORY_JUDGE_PROMPT.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user_input.to_string(),
        },
    ];

    let raw = chat::complete_structured(
        &messages,
        &extract_model_or_default(),
        "memory_judgement",
        judgement_schema(),
    )?;

    parse_judgement(&raw)
}

/// 第二步：把原话归一成一条第三人称事实句（纯文本输出）。
fn summarize(user_input: &str) -> Option<String> {
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: MEMORY_SUMMARY_PROMPT.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user_input.to_string(),
        },
    ];

    let raw = chat::complete_with_model(&messages, &extract_model_or_default()).ok()?;
    let text = clean_summary(&raw);
    (!text.is_empty()).then_some(text)
}

/// 用于两步调用的模型：`LLM_EXTRACT_MODEL` 优先，留空复用对话模型。
fn extract_model_or_default() -> String {
    let m = Config::get().llm_extract_model.trim();
    if m.is_empty() {
        Config::get().llm_model.clone()
    } else {
        m.to_string()
    }
}

/// 判定结果的 JSON Schema：枚举字段用 `enum` 收窄——支持 `json_schema` 的端点
/// （llama.cpp 系）会以 grammar 约束解码，不可能解出表外值。
fn judgement_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "is_memory": { "type": "boolean" },
            "memory_type": {
                "type": "string",
                "enum": ["fact", "preference", "personal", "todo", "event"]
            },
            "tier": { "type": "string", "enum": ["short", "intent", "core"] },
            "relation": {
                "type": "string",
                "enum": ["neutral", "warm", "ambig", "conflict", "reconcile"]
            }
        },
        "required": ["is_memory", "memory_type", "tier", "relation"]
    })
}

/// 判定标签：第一步调用的产物（不含事实化文本，那是第二步的事）。
#[derive(Debug, Clone)]
struct Judgement {
    is_memory: bool,
    memory_type: String,
    tier: String,
    relation: String,
}

impl Default for Judgement {
    fn default() -> Self {
        Self {
            is_memory: false,
            memory_type: default_memory_type(),
            tier: default_tier(),
            relation: default_relation(),
        }
    }
}

/// 解析判定输出。
///
/// 模型输出一律当作**不可信输入**，以下情况都不算错误，各自回退默认值：
/// 外层 markdown 代码围栏、前后多余文本、尾部追加了第二个 JSON 对象、
/// 字段缺失、字段为 `null`、字段类型不符（如 `"is_memory": "true"`）、空白串。
///
/// 真正报错的只有一种：整段输出里找不到任何可解析的 JSON 对象。
fn parse_judgement(raw: &str) -> Result<Judgement> {
    let value = find_json_object(raw).ok_or_else(|| {
        SqlError::Config(format!(
            "LLM 判定输出不包含可解析的 JSON 对象; 原始输出: {raw}"
        ))
    })?;
    let Some(obj) = value.as_object() else {
        return Ok(Judgement::default());
    };

    Ok(Judgement {
        is_memory: bool_field(obj.get("is_memory")),
        memory_type: str_field(obj.get("memory_type"), default_memory_type),
        tier: str_field(obj.get("tier"), default_tier),
        relation: str_field(obj.get("relation"), default_relation),
    })
}

/// 清理事实化输出：事实化要的是纯文本，但模型偶尔仍会套围栏、加引号、
/// 或把字段名一起写出来。这里都剥掉；句子内部的标点保持原样。
fn clean_summary(raw: &str) -> String {
    let text = strip_code_fences(raw);
    let text = text.trim();

    // 去掉模型偶尔带上的字段名前缀（`memory_content: 对方…`）。
    let text = text
        .split_once([':', '：'])
        .filter(|(prefix, _)| {
            matches!(
                prefix.trim().to_ascii_lowercase().as_str(),
                "memory_content" | "memory_summary" | "content" | "sentence"
            )
        })
        .map_or(text, |(_, rest)| rest.trim());

    // 去掉整体包裹的引号。
    let text = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| text.strip_prefix('“').and_then(|s| s.strip_suffix('”')))
        .or_else(|| text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(text);

    text.trim().to_string()
}

/// 宽容布尔：`true` / `"true"` / `"1"` / 非零数字 视为真；缺失、`null`、其余视为假。
fn bool_field(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => {
            matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        }
        Some(serde_json::Value::Number(n)) => n.as_i64().is_some_and(|i| i != 0),
        _ => false,
    }
}

/// 宽容取字符串：只接受非空白字符串，其余（缺失 / `null` / 数字 / 空白）回退默认值。
fn str_field(value: Option<&serde_json::Value>, default: fn() -> String) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(default)
}

/// 抠出第一个可解析的 JSON 对象，容忍代码围栏与多余文本。
fn find_json_object(raw: &str) -> Option<serde_json::Value> {
    let cleaned = strip_code_fences(raw);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned) {
        if v.is_object() {
            return Some(v);
        }
    }
    // 退化路径：从第一个 '{' 起，从后往前缩右边界，取首个能解析的候选
    // （应对模型在 JSON 后面又补了一段文字或第二个对象）。
    let rest = &cleaned[cleaned.find('{')?..];
    rest.match_indices('}')
        .rev()
        .find_map(|(idx, _)| serde_json::from_str::<serde_json::Value>(&rest[..=idx]).ok())
        .filter(serde_json::Value::is_object)
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

/// 第一步（判定）的系统提示词：英文直接给 LLM，中文注释供开发者阅读。
///
/// 只做分类，不产出任何自然语言内容——这是本步稳定的关键。
/// is_memory 为 true 仅限实质信息（偏好/计划/经历），闲聊一律 false；
/// memory_type 为 fact / preference / personal / todo / event；
/// tier 为 short（临时易失效）/ intent（计划/进行中）/ core（长期事实与偏好）；
/// relation 为 neutral（日常闲聊）/ warm（关心/惦记）/ ambig（暧昧/亲密）/
/// conflict（矛盾/吵架/失望/误会）/ reconcile（和解/道歉），只按本条消息的
/// 语气判定，无情绪的普通对话必须 neutral。
const MEMORY_JUDGE_PROMPT: &str = r#"
You are a conversational-memory classifier. Given ONE user message, output a single compact JSON object with four labels.

Required output — strict JSON only, no extra text, no markdown fences, no comments:
{"is_memory": false, "memory_type": "fact", "tier": "core", "relation": "neutral"}

Fields:
- is_memory: TRUE only when the message carries substantive information worth recalling later (personal info, preferences, important experiences, plans, task progress). FALSE for casual small talk, plain questions, greetings, repetitive complaints with nothing new, pure venting without concrete facts. When unsure, FALSE — rather skip than over-store.
- memory_type: one of "fact" | "preference" | "personal" | "todo" | "event". Use "fact" when unsure.
- tier: how long the fact stays valid.
  - "short": temporary state that passes soon (e.g. "我今天身体不舒服", "在赶一个项目", "感冒还没好").
  - "intent": ongoing plan/intention, fairly stable but subject to change (e.g. "打算下个月去看电影", "下周要考试", "准备搬家").
  - "core": long-term stable facts and preferences (e.g. "最喜欢蓝色", "养了一只叫豆豆的猫").
- relation: the ONE relation event this message produces, judged by its tone alone.
  - "neutral": ordinary small talk, plain questions, routine chat; no emotional charge. DEFAULT when unsure.
  - "warm": caring, thinking of the other, heartwarming, gratitude (e.g. "记得按时吃饭", "谢谢你一直陪着我").
  - "ambig": flirtatious, testing, romantic or intimate undercurrent (e.g. "有点想你了", "你觉得我这个人怎么样", 打情骂俏).
  - "conflict": disagreement, argument, complaint, disappointment, blame, sulking, misunderstanding (e.g. "你总是这样敷衍我", "你根本不懂我").
  - "reconcile": apology, making up, extending an olive branch, clearing a misunderstanding (e.g. "刚才是我不对", "别生气了嘛").

relation is independent of is_memory: a message may be memory-worthy, relation-relevant, both, or neither. Pick the single most salient event of THIS message.
Always fill every field with one of the values listed above — never output null.
"#;

/// 第二步（事实化）的系统提示词：只在判定为"值得记住"后调用。
///
/// 输出是**纯文本一句话**（不是 JSON）：第三人称、以"对方"作主语、只留一个核心事实，
/// 禁止"我/我们"（引语除外），不虚构输入中不存在的信息。
const MEMORY_SUMMARY_PROMPT: &str = r#"
Rewrite ONE user message as a single normalized factual sentence to be stored in a long-term memory bank.

Output rules:
- Output ONE sentence in plain text (Chinese). No JSON, no quotes around it, no field names, no explanation, no markdown fences.
- Keep exactly one core fact. Strip colloquial filler, greetings, emotional venting and repetitions.
- Keep concrete details (time, place, names, numbers) when they matter.
- Person normalization: no matter how the user refers to themselves, the subject must be "对方". Never use first-person pronouns ("我/我们/俺/咱们/本人") except inside a verbatim quote.
- Never invent information that is not in the message.

Examples:
- "我最喜欢蓝色了" → 对方最喜欢的颜色是蓝色
- "下周要考试，好紧张" → 对方下周要考试
- "最近在赶一个项目，天天加班到十点" → 对方最近在赶一个项目，经常加班到十点
"#;

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 第一步：判定标签的宽容解析 ----------

    #[test]
    fn parse_judgement_reads_labels() {
        let raw = r#"{"is_memory": true, "memory_type": "preference", "tier": "short", "relation": "warm"}"#;
        let parsed = parse_judgement(raw).unwrap();
        assert!(parsed.is_memory);
        assert_eq!(parsed.memory_type, "preference");
        assert_eq!(parsed.tier, "short");
        assert_eq!(parsed.relation, "warm");
    }

    #[test]
    fn parse_judgement_defaults_when_field_missing() {
        let raw = r#"{"is_memory": false, "memory_type": "fact", "tier": "core"}"#;
        let parsed = parse_judgement(raw).unwrap();
        assert!(!parsed.is_memory);
        assert_eq!(parsed.relation, "neutral");
    }

    #[test]
    fn parse_judgement_tolerates_null_fields() {
        // 实测模型输出：判定为"不是记忆"时把 tier 写成 null，不能因此报错。
        let raw = r#"{
  "is_memory": false,
  "memory_type": "fact",
  "tier": null,
  "relation": "conflict"
}"#;
        let parsed = parse_judgement(raw).unwrap();
        assert!(!parsed.is_memory);
        assert_eq!(parsed.tier, "core");
        assert_eq!(parsed.relation, "conflict");
    }

    #[test]
    fn parse_judgement_tolerates_null_and_blank_enum_fields() {
        let raw = r#"{"is_memory": null, "memory_type": null, "tier": "  ", "relation": null}"#;
        let parsed = parse_judgement(raw).unwrap();
        assert!(!parsed.is_memory);
        assert_eq!(parsed.memory_type, "fact");
        assert_eq!(parsed.tier, "core");
        assert_eq!(parsed.relation, "neutral");
    }

    #[test]
    fn parse_judgement_tolerates_wrong_types() {
        // 弱约束端点（json_object）可能把布尔写成字符串、把枚举写成数字。
        let raw = r#"{"is_memory": "true", "memory_type": "preference", "tier": 3, "relation": "warm"}"#;
        let parsed = parse_judgement(raw).unwrap();
        assert!(parsed.is_memory, "字符串 \"true\" 应视为真");
        assert_eq!(parsed.memory_type, "preference");
        assert_eq!(parsed.tier, "core", "类型不符的枚举回退默认");
        assert_eq!(parsed.relation, "warm");
    }

    #[test]
    fn parse_judgement_tolerates_code_fences() {
        let raw = "```json\n{\"is_memory\": true, \"memory_type\": \"todo\", \"tier\": \"intent\", \"relation\": \"neutral\"}\n```";
        let parsed = parse_judgement(raw).unwrap();
        assert!(parsed.is_memory);
        assert_eq!(parsed.memory_type, "todo");
        assert_eq!(parsed.tier, "intent");
    }

    #[test]
    fn parse_judgement_picks_first_object_when_model_appends_more() {
        // 模型在 JSON 之后又补了第二个对象：应取第一个。
        let raw = "{\"is_memory\": false, \"memory_type\": \"fact\", \"tier\": \"short\", \"relation\": \"neutral\"}\n{\"extra\": true}";
        let parsed = parse_judgement(raw).unwrap();
        assert!(!parsed.is_memory);
        assert_eq!(parsed.tier, "short");
    }

    #[test]
    fn parse_judgement_errors_only_when_no_json_object() {
        // 唯一仍然报错的情形：整段输出里没有任何 JSON 对象。
        let err = parse_judgement("抱歉，我无法判断。").unwrap_err();
        assert!(err.to_string().contains("不包含可解析的 JSON 对象"), "{err}");
    }

    #[test]
    fn unknown_relation_label_falls_back_to_neutral() {
        let raw = r#"{"is_memory": false, "memory_type": "fact", "tier": "core", "relation": "???whatever"}"#;
        let parsed = parse_judgement(raw).unwrap();
        assert_eq!(
            RelationEvent::from_label(&parsed.relation),
            RelationEvent::Neutral
        );
    }

    // ---------- 第二步：事实化文本的清理 ----------

    #[test]
    fn clean_summary_strips_fences_quotes_and_field_prefix() {
        assert_eq!(
            clean_summary("```\n对方最喜欢的颜色是蓝色\n```"),
            "对方最喜欢的颜色是蓝色"
        );
        assert_eq!(clean_summary("\"对方下周要考试\""), "对方下周要考试");
        assert_eq!(
            clean_summary("“对方在下雨天会情绪低落”"),
            "对方在下雨天会情绪低落"
        );
        assert_eq!(
            clean_summary("memory_content: 对方在准备考研"),
            "对方在准备考研"
        );
    }

    #[test]
    fn clean_summary_keeps_sentence_intact() {
        // 句内的冒号属于内容本身，不能被当成字段名前缀剥掉。
        assert_eq!(
            clean_summary("对方给自己的猫起了名字：豆豆"),
            "对方给自己的猫起了名字：豆豆"
        );
        assert_eq!(clean_summary("  对方最近在赶项目  "), "对方最近在赶项目");
        assert_eq!(clean_summary(""), "");
    }
}
