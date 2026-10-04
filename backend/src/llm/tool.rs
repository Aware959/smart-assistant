//! 工具目录：模型可调用的函数定义与派发。
//!
//! 分两类，职责完全不同：
//!
//! - **输出工具** [`SEND_MESSAGE`]：唯一对外出口。模型想说的话只能走这里，
//!   调用即投递，返回给模型的确认文本不参与任何判断。
//! - **只读工具** [`GET_NOW`] / [`RECALL_MEMORY`]：只取上下文，不改状态。
//!   调用结果回填给模型继续推理。
//!
//! 终止判据**不依赖任何工具**：一轮响应没有 `tool_calls` 即结束
//! （与 OpenAI Agents SDK / Anthropic 的 `stop_reason != tool_use` 同构）。
//! 曾评估过 `finish` 工具做乐观终止，实测命中率仅 1/10，不足以承担判据。

use genai::chat::Tool;

use crate::core::schema::message::ChatCall;

/// 对外输出工具：模型调用它即向用户投递一条消息。
pub const SEND_MESSAGE: &str = "send_message";
/// 只读工具：取当前本地时间。
pub const GET_NOW: &str = "get_now";
/// 只读工具：按改写后的 query 检索长期记忆。
pub const RECALL_MEMORY: &str = "recall_memory";

/// 单条消息的长度上限（超出即拒绝，防止模型把整段话塞进一次调用）。
pub const SEND_MESSAGE_MAX_CHARS: usize = 200;

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

/// 只读工具的执行环境。抽成 trait 便于测试注入确定性时钟与固定记忆。
pub trait ToolContext {
    /// 当前本地时间，形如 `2026-10-03 14:30:00 星期六`。
    fn now(&self) -> String;
    /// 按 query 检索记忆，返回可直接回填给模型的描述文本（无命中时给出明确说明）。
    fn recall(&self, query: &str) -> String;
}

/// 全部工具的 schema 定义。每次请求都带上，不做动态裁剪——
/// 三个工具的描述都很短，条件裁剪省下的 token 不值得换来"模型不知道工具存在"的风险。
pub fn definitions() -> Vec<Tool> {
    vec![send_message_tool(), get_now_tool(), recall_memory_tool()]
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
            "检索用户的长期记忆。需要回忆用户的偏好、习惯、称呼、经历、约定等\
             之前说过的事情时调用。不确定用户相关信息时先查，不要凭空猜测。",
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

/// 执行一次工具调用。
pub fn dispatch(call: &ChatCall, ctx: &dyn ToolContext) -> ToolOutcome {
    match call.fn_name.as_str() {
        SEND_MESSAGE => match call.arg_str("text") {
            Some(raw) => {
                let text = crate::utils::textflow::flatten(raw);
                let n = text.chars().count();
                if n > SEND_MESSAGE_MAX_CHARS {
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
        other => ToolOutcome::Rejected(format!("未知工具 {other}")),
    }
}

/// `send_message` 成功投递后回填给模型的确认文本。
///
/// 刻意极短：这段文本会进入下一轮 prompt，啰嗦会被模型模仿成对用户的废话。
pub const SEND_MESSAGE_ACK: &str = "已发送";
