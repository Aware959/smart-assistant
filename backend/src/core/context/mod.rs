//! 上下文组装：把"历史消息 / 召回记忆"组装成一轮对话或决策所需的上下文。
//!
//! 只读、无副作用——只消费能力端口，不改动任何状态。此前这套逻辑散落在
//! `core::prompt`（对话路径）与 `intent` 的决策路径两处、各拼一份，
//! 现在统一收在本模块，两条路径共用同一套取数与渲染。
//!
//! **调用方仍需自己掌握取数时机**（两条路径时序不同，不强行做成单一入口）：
//! - 对话路径：历史必须在「本条用户消息入库之前」取（否则当前消息会被算进历史）；
//! - 决策路径：没有"当前消息"，召回用最近历史文本做 query。

pub(crate) mod history;
pub(crate) mod prompt;
pub(crate) mod recall;

pub(crate) use history::{fetch as fetch_history, USER_OFFSET_MINUTES};
pub(crate) use prompt::{persona_block, system_prompt};
pub(crate) use recall::{fetch as fetch_recall, plain_lines, RECALL_LIMIT};

use crate::core::memory::MemoryHit;
use crate::core::schema::message::ChatMessage;
use crate::core::session::Sessions;

/// 一轮对话 / 决策所需的上下文：由本模块只读组装。
///
/// 把"历史消息 / 召回记忆"两个来源收拢成一份可移植值，
/// 让对话流水线与主动决策共用同一套组装结果，而不是各自手拼一份。
#[derive(Debug, Clone, Default)]
pub struct AgentContext {
    /// 历史消息轮次（对话路径按"今天"时间线整理并压缩；决策路径为最近 N 条）。
    pub history: Vec<ChatMessage>,
    /// 与当前话题相关的记忆召回（已过滤相关度、剔除过期；不召回时为空）。
    pub memories: Vec<MemoryHit>,
}

/// 最近 `limit` 条对话的渲染文本（供主动决策与延续判断阅读）。
///
/// 与对话路径不同，这里只取"最近 N 条"而非按天时间线；取不到消息时返回空串，
/// 调用方据此跳过本轮（主动推送不该因取历史失败而报错）。
pub(crate) fn recent_history_text(
    conversation: &Sessions,
    session_id: &str,
    limit: usize,
) -> String {
    match history::fetch_recent(conversation, session_id, limit) {
        Ok(turns) => prompt::history_text(&turns),
        Err(_) => String::new(),
    }
}
