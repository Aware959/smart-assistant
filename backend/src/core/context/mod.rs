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
pub(crate) use prompt::{
    local_time_text, proactive_system_prompt, proactive_user_prompt, system_prompt,
};
pub(crate) use recall::{id_lines, RECALL_LIMIT};

use crate::core::session::Sessions;

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
