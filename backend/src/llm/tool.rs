//! 工具目录：模型可调用的函数定义与派发。
//!
//! 分三类，职责完全不同：
//!
//! - **输出工具** [`SEND_MESSAGE`]：唯一对外出口。模型想说的话只能走这里，
//!   调用即投递，返回给模型的确认文本不参与任何判断。
//! - **只读工具** [`GET_NOW`] / [`RECALL_MEMORY`]：只取上下文，不改状态。
//!   调用结果回填给模型继续推理。
//! - **写入工具** [`REMEMBER`]：模型主动沉淀一条长期记忆。改状态，但改的是
//!   助手的记忆而不是用户可见的输出，所以归在"内部"这一侧。
//!
//! 终止判据**不依赖任何工具**：一轮响应没有 `tool_calls` 即结束
//! （与 OpenAI Agents SDK / Anthropic 的 `stop_reason != tool_use` 同构）。
//! 曾评估过 `finish` 工具做乐观终止，实测命中率仅 1/10，不足以承担判据。

use genai::chat::Tool;

use crate::core::memory::{MemoryKind, WriteKind};
use crate::core::schema::message::ChatCall;

/// 对外输出工具：模型调用它即向用户投递一条消息。
pub const SEND_MESSAGE: &str = "send_message";
/// 只读工具：取当前本地时间。
pub const GET_NOW: &str = "get_now";
/// 只读工具：按改写后的 query 检索长期记忆。
pub const RECALL_MEMORY: &str = "recall_memory";
/// 写入工具：沉淀一条长期记忆。
pub const REMEMBER: &str = "remember";

/// 单条消息的长度上限（超出即拒绝，防止模型把整段话塞进一次调用）。
pub const SEND_MESSAGE_MAX_CHARS: usize = 200;

/// 记忆内容的长度上限：自足的事实陈述不需要长到这个程度，超了基本是在抄原文。
pub const REMEMBER_MAX_CHARS: usize = 200;

/// 工具派发结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome {
    /// 输出工具产出的对外文本（已按上限裁剪）。
    Sent(String),
    /// 只读工具产出的回填文本。
    Filled(String),
    /// 调用非法（未知工具名 / 参数不合约），回填给模型让它自行纠正。
    Rejected(String),
}

/// 工具的执行环境。抽成 trait 便于测试注入确定性时钟与固定记忆。
pub trait ToolContext {
    /// 当前本地时间，形如 `2026-10-03 14:30:00 星期六`。
    fn now(&self) -> String;
    /// 按 query 检索记忆，返回可直接回填给模型的描述文本（无命中时给出明确说明）。
    fn recall(&self, query: &str) -> String;
    /// 写入一条记忆。`kind` 是模型判定的种类（决定默认寿命），`supersedes` 为要改写的
    /// 既有记忆 id（来自 [`ToolContext::recall`]）。
    fn remember(
        &self,
        content: &str,
        kind: MemoryKind,
        supersedes: Option<&str>,
    ) -> std::result::Result<WriteKind, String>;
}

/// 全部工具的 schema 定义。每次请求都带上，不做动态裁剪——
/// 四个工具的描述都很短，条件裁剪省下的 token 不值得换来"模型不知道工具存在"的风险。
pub fn definitions() -> Vec<Tool> {
    vec![
        send_message_tool(),
        get_now_tool(),
        recall_memory_tool(),
        remember_tool(),
    ]
}

fn send_message_tool() -> Tool {
    Tool::new(SEND_MESSAGE)
        .with_description(
            "向用户发送一条消息。这是你唯一能让用户看到你说话的方式。\
             一次调用只发一条消息；要说多条就多次调用。\
             每条保持简短自然，不要换行、不要markdown、不要把多条内容合并进一次调用。",
        )
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "要发给用户的这条消息的内容"
                }
            },
            "required": ["text"],
            "additionalProperties": false
        }))
}

fn get_now_tool() -> Tool {
    Tool::new(GET_NOW)
        .with_description("获取当前的本地日期和时间。需要知道现在几点、今天几号、星期几时调用。")
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }))
}

fn recall_memory_tool() -> Tool {
    Tool::new(RECALL_MEMORY)
        .with_description(
            "检索用户的长期记忆，返回结果带 [id]，后续可用 remember 的 supersedes 指向它。\
             需要回忆用户的偏好、习惯、称呼、经历、约定等之前说过的事情时调用。\
             不确定用户相关信息时先查，不要凭空猜测。",
        )
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "检索用的陈述式查询短语，如「用户偏好的咖啡口味」"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }))
}

/// 记忆准入标准写在这里，而不是系统提示词里。
///
/// 三个理由：工具描述**每轮都在上下文里**，不占提示词的注意力预算；它和参数
/// 声明在同一处，模型读"怎么用"和"什么时候用"时不用来回跳；而写入判据若交给
/// 一段提示词，模型很容易在长对话里把它当成软建议——工具描述是契约，读起来更像。
///
/// 三道闸按顺序过，任一条不过就不记：
/// 1. **是不是关于对方**：只记用户的处境、偏好、经历、约定。"你应该怎么回我"
///    这类关于助手自身行为的要求不是用户记忆；
/// 2. **是不是自足**：一句话脱离当前对话也能读懂。"那个任务还没定"里的"那个"
///    三个月后没人知道指什么；问句、言语行为（"他自称傲娇"）同样不是事实；
/// 3. **丢了会不会说错**：只有"忘了就会答错、或只能给一个泛泛的答案"才值得记。
///    "以后可能有用"不构成理由——记下的每条都会在将来某轮被塞进上下文，
///    噪声的代价由那时正在说话的你来付。
fn remember_tool() -> Tool {
    Tool::new(REMEMBER)
        .with_description(
            "记住一条关于用户的长期事实，以后需要时会自动想起。\
             先判断它属于哪一类，kind 是必填的——种类决定它能活多久，选错会被过期：\
             trait=稳定的性格/能力/身份（会一点厨艺、自称傲娇、名字是阿离），永不过期；\
             preference=稳定的偏好与对 AI 的指令（希望被叫阿离、想念时大胆说），永不过期；\
             routine=作息与时段规律（午休 12 点到 2 点），90 天内没再确认就淡出；\
             plan=有时效的意向与安排（打算下个月看电影），30 天不落实就淡出。\
             四类都不沾就别记——「以后也许用得上」不是理由，宁可漏记也不要存成噪音。\
             内容还要同时满足三条：\
             1. 是关于用户本人的处境、偏好、经历或约定，不是关于你该怎么回话；\
n             2. 是一句自足的话——脱离当前对话也能读懂，不能有「那个」「上次说的」这类悬空指代，\
                 也不能是问句或「他自称…」这类转述；\
             3. 忘了它就会让你答错或只能泛泛而谈。\
             内容用自己的话写成一两句完整陈述，别抄用户原话。\
             用户的话被推翻或更新时（搬家、换工作、换称呼），用 recall_memory 查到旧记忆，\
             再把它的 [id] 填进 supersedes 改写掉那条，不要让新旧两条并存。",
        )
        .with_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": MemoryKind::ALL.map(|k| k.as_str()),
                    "description": "这条记忆的种类：trait / preference / routine / plan。决定默认有效期"
                },
                "content": {
                    "type": "string",
                    "description": "要记住的自足事实陈述，如「用户现在住在上海，之前住北京」"
                },
                "supersedes": {
                    "type": "string",
                    "description": "被这条内容取代的既有记忆 id，来自 recall_memory 结果里的 [id]。\
                                    没有需要取代的旧记忆时省略这个参数。"
                }
            },
            "required": ["kind", "content"],
            "additionalProperties": false
        }))
}

/// 执行一次工具调用。
pub fn dispatch(call: &ChatCall, ctx: &dyn ToolContext) -> ToolOutcome {
    match call.fn_name.as_str() {
        SEND_MESSAGE => match call.arg_str("text") {
            Some(raw) => {
                let text = crate::utils::textflow::flatten(raw);
                let n = text.chars().count();
                if n == 0 {
                    // 归一化后为空 = 这一轮没攒出可说的话。投递空消息在通道上是一次
                    // 无意义的推送，比不投更像故障；回填说明让它自己决定要不要补话。
                    ToolOutcome::Filled("没有可发送的内容，这一轮什么都没说。".to_string())
                } else if n > SEND_MESSAGE_MAX_CHARS {
                    // 不截断、也不投递：截断会留下半句话，而半句话比长句更像故障。
                    // 直接要求模型自己拆成多条——它能决定在哪断。
                    ToolOutcome::Filled(format!(
                        "这条有 {n} 字，超过单条 {SEND_MESSAGE_MAX_CHARS} 字上限，没有发送。请拆成多条分别调用。"
                    ))
                } else {
                    ToolOutcome::Sent(text)
                }
            }
            None => ToolOutcome::Rejected("缺少参数 text".to_string()),
        },
        GET_NOW => ToolOutcome::Filled(ctx.now()),
        RECALL_MEMORY => match call.arg_str("query") {
            Some(q) if !q.trim().is_empty() => ToolOutcome::Filled(ctx.recall(q.trim())),
            _ => ToolOutcome::Rejected("缺少参数 query".to_string()),
        },
        REMEMBER => match call.arg_str("content") {
            Some(raw) if !raw.trim().is_empty() => {
                let content = raw.trim();
                // 先校验 kind：内容有问题时不该让模型误以为是种类填错了。
                let Some(kind) = parse_kind(call.arg_str("kind")) else {
                    return ToolOutcome::Rejected(format!(
                        "缺少或非法的参数 kind，应为 {} 之一",
                        MemoryKind::ALL.map(|k| k.as_str()).join(" / ")
                    ));
                };
                if content.chars().count() > REMEMBER_MAX_CHARS {
                    ToolOutcome::Rejected(format!(
                        "这条有 {} 字，超过单条 {REMEMBER_MAX_CHARS} 字上限，没有记住。一条事实不该这么长，写短一点。",
                        content.chars().count()
                    ))
                } else {
                    // 空串按"没传"处理：模型偶尔会给 supersedes 塞个空串。
                    let supersedes = call
                        .arg_str("supersedes")
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    match ctx.remember(content, kind, supersedes) {
                        Ok(WriteKind::Created) => ToolOutcome::Filled(REMEMBER_ACK_NEW.to_string()),
                        Ok(WriteKind::Revised) => {
                            ToolOutcome::Filled(REMEMBER_ACK_REVISED.to_string())
                        }
                        Err(e) => ToolOutcome::Rejected(format!("没有记住：{e}")),
                    }
                }
            }
            _ => ToolOutcome::Rejected("缺少参数 content".to_string()),
        },
        other => ToolOutcome::Rejected(format!("未知工具 {other}")),
    }
}

/// 解析 `kind` 参数；缺失或不在枚举内返回 None，由调用方给模型明确回执。
fn parse_kind(raw: Option<&str>) -> Option<MemoryKind> {
    let raw = raw?.trim();
    MemoryKind::ALL
        .into_iter()
        .find(|k| k.as_str() == raw.to_ascii_lowercase())
}

/// `send_message` 成功投递后回填给模型的确认文本。
///
/// 刻意极短：这段文本会进入下一轮 prompt，啰嗦会被模型模仿成对用户的废话。
pub const SEND_MESSAGE_ACK: &str = "已发送";

/// `remember` 新增一条记忆后的回填文本。同样极短，且不复述内容——
/// 让模型把自己的话再抄一遍进下一轮 prompt，只会诱发它接着解释"我记住了…"。
pub const REMEMBER_ACK_NEW: &str = "已记住";

/// `remember` 改写既有记忆后的回填文本。
pub const REMEMBER_ACK_REVISED: &str = "已改写旧记忆";

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tool_tests;
