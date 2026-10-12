//! 历史消息 → 对话上下文：按"今天"的时间线取消息并整理。
//!
//! 直接消费 [`crate::core::session::Sessions`]（会话在 core 内是具体类型，不再是端口）。

use chrono::{DateTime, Duration, Utc};

use crate::core::schema::message::{ChatCall, ChatMessage};
use crate::core::session::MessageTurn;
use crate::core::session::Sessions;
use crate::error::Result;

/// 一天的起点钟点（用户当地时钟）：这个钟点之前算昨天、之后算今天。
const DAY_START_HOUR: u32 = 6;
/// 用户当地时区与 UTC 的偏移（分钟）：日界兜底用东八区。
/// 此前由世界状态运行时按画像动态推断，aiworld 已拆、等待重新设计，暂用固定常量。
pub(crate) const USER_OFFSET_MINUTES: i64 = 8 * 60;
/// 今日消息少于该条数时，把昨天也并入上下文（人记得昨晚的事）。
const DAY_POOL_THRESHOLD: usize = 6;
/// 按天上下文的最大消息条数；超出时保留开头与结尾、中间整段省略。
const DAY_CONTEXT_MAX: usize = 60;
/// 压缩时保留的头部条数（保住今天早晨的开场）。
const DAY_KEEP_HEAD: usize = 8;

/// 按用户当地时钟计算"今天"的起点（日界 = 当地 06:00，返回世界时 UTC 时刻）。
pub(crate) fn day_window_start(offset_minutes: i64) -> DateTime<Utc> {
    (Utc::now() + Duration::minutes(offset_minutes))
        .date_naive()
        .and_hms_opt(DAY_START_HOUR, 0, 0)
        .expect("valid clock time")
        .and_utc()
        - Duration::minutes(offset_minutes)
}

/// 取一轮对话要用的历史消息。
///
/// - `history_count` 为 `None`：按"今天"取（日界 = 用户当地 06:00；今日过少自动并入
///   昨天，让 AI 有连续性），并做超长压缩；
/// - `Some(n)`：退化为取最近 N 条纯文本。
///
/// **调用时机**：必须在"本条用户消息入库之前"取，否则当前消息会被算进历史。
pub(crate) fn fetch(
    conversation: &Sessions,
    session_id: &str,
    history_count: Option<u32>,
) -> Result<Vec<ChatMessage>> {
    match history_count {
        Some(n) => fetch_recent(conversation, session_id, n as usize),
        None => {
            let start = day_window_start(USER_OFFSET_MINUTES);
            let mut turns = conversation.messages_since(session_id, start)?;
            if turns.len() < DAY_POOL_THRESHOLD {
                turns = conversation.messages_since(session_id, start - Duration::days(1))?;
            }
            build_day_history(&turns)
        }
    }
}

/// 取最近 `limit` 条历史（时间正序），不做按天整理。
///
/// 主动决策 / 延续判断用这条路径：它们看的是"刚刚聊到哪了"，不需要跨天铺陈。
pub(crate) fn fetch_recent(
    conversation: &Sessions,
    session_id: &str,
    limit: usize,
) -> Result<Vec<ChatMessage>> {
    Ok(conversation
        .recent_messages(session_id, limit)?
        .iter()
        .map(turn_to_chat)
        .collect())
}

/// 按"今天"把历史消息构建成对话上下文（参考人的作息与生物节律）：
/// - 入场条数超上限时做骨架压缩（保留开头与结尾、省略中间），控制单轮上下文长度；
/// - 说话人靠消息本身的 role 一并传给模型，内容保持原文、不做任何装饰。
pub(crate) fn build_day_history(turns: &[MessageTurn]) -> Result<Vec<ChatMessage>> {
    let total = turns.len();
    let compact = total > DAY_CONTEXT_MAX;
    let tail_start = total.saturating_sub(DAY_CONTEXT_MAX - DAY_KEEP_HEAD);

    let mut out = Vec::with_capacity(total.min(DAY_CONTEXT_MAX));
    for (idx, m) in turns.iter().enumerate() {
        if compact && idx >= DAY_KEEP_HEAD && idx < tail_start {
            continue;
        }
        out.push(turn_to_chat(m));
    }
    Ok(out)
}

/// 把一条持久化历史轮次还原成发给模型的 [`ChatMessage`]，拼回工具调用上下文。
///
/// genai 要求 assistant 轮的工具调用必须紧跟对应的 tool 结果；工具结果在库里沿用
/// `role='assistant'` + `tool_call_id` 标记（避免改动 messages 的 role CHECK 约束），
/// 这里据 `tool_call_id` 非空还原成 `role='tool'`，据 `tool_calls` 的 JSON 还原工具调用列表。
fn turn_to_chat(turn: &MessageTurn) -> ChatMessage {
    let mut msg = ChatMessage::new(turn.role.clone(), turn.content.clone());
    if let Some(json) = &turn.tool_calls {
        if let Ok(calls) = serde_json::from_str::<Vec<ChatCall>>(json) {
            msg.tool_calls = calls;
        }
    }
    msg.tool_call_id = turn.tool_call_id.clone();
    msg.tool_name = turn.tool_name.clone();
    // tool 结果行：库里 role 仍是 assistant，按 tool_call_id 还原协议角色。
    if msg.tool_call_id.is_some() {
        msg.role = "tool".to_string();
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::MessageTurn;
    use chrono::Timelike;

    const EAST8_OFFSET: i64 = 8 * 60;

    fn turn(role: &str, content: &str, created_at: &str) -> MessageTurn {
        MessageTurn {
            role: role.to_string(),
            content: content.to_string(),
            created_at: created_at.to_string(),
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
        }
    }

    fn today_6am_utc() -> DateTime<Utc> {
        let now = Utc::now();
        (now + Duration::minutes(EAST8_OFFSET))
            .date_naive()
            .and_hms_opt(DAY_START_HOUR, 0, 0)
            .unwrap()
            .and_utc()
            - Duration::minutes(EAST8_OFFSET)
    }

    fn rfc(dt: DateTime<Utc>) -> String {
        dt.to_rfc3339()
    }

    #[test]
    fn day_history_preserves_raw_content_and_native_roles() {
        let base = today_6am_utc();
        let turns = vec![
            turn("user", "早上好", &rfc(base + Duration::minutes(30))),
            turn("assistant", "早呀", &rfc(base + Duration::minutes(31))),
            turn("user", "午安", &rfc(base + Duration::minutes(211))),
        ];

        let out = build_day_history(&turns).unwrap();
        assert_eq!(out.len(), 3);
        // 说话人只靠原生 role 表达，内容保持原文，不带任何时间戳/角色前缀装饰。
        assert_eq!(out[0].role, "user");
        assert_eq!(out[0].content, "早上好");
        assert_eq!(out[1].role, "assistant");
        assert_eq!(out[1].content, "早呀");
        assert_eq!(out[2].role, "user");
        assert_eq!(out[2].content, "午安");
    }

    #[test]
    fn day_history_compacts_middle_when_over_capacity() {
        let base = today_6am_utc();
        let turns = (0..(DAY_CONTEXT_MAX + 10))
            .map(|i| {
                let role = if i % 2 == 0 { "user" } else { "assistant" };
                turn(
                    role,
                    &format!("消息{i}"),
                    &rfc(base + Duration::minutes(i as i64)),
                )
            })
            .collect::<Vec<_>>();
        let tail_start = turns.len() - (DAY_CONTEXT_MAX - DAY_KEEP_HEAD);

        let out = build_day_history(&turns).unwrap();
        assert_eq!(out.len(), DAY_CONTEXT_MAX);
        // 保留开头与结尾，中间整段省略。
        assert_eq!(out[0].content, "消息0");
        assert_eq!(out[1].content, "消息1");
        assert_eq!(out[DAY_KEEP_HEAD].content, format!("消息{tail_start}"));
        assert_eq!(
            out.last().unwrap().content,
            format!("消息{}", turns.len() - 1)
        );
    }

    #[test]
    fn day_window_start_is_local_6am_utc_converted() {
        let start = day_window_start(EAST8_OFFSET);
        // 世界时应等于"当地 06:00 的时刻减 8 小时"。
        let local = start + Duration::minutes(EAST8_OFFSET);
        assert_eq!(local.hour(), DAY_START_HOUR);
        assert_eq!(local.minute(), 0);
    }

    #[test]
    fn turn_to_chat_restores_tool_roundtrip() {
        // assistant 轮携带工具调用列表，应还原成 role='assistant' + tool_calls。
        let calls = serde_json::to_string(&vec![crate::core::schema::message::ChatCall::new(
            "c1",
            "recall_memory",
            serde_json::json!({"q": "爱好"}),
        )])
        .unwrap();
        let assistant = turn("assistant", "稍等", &rfc(Utc::now()));
        let assistant = MessageTurn {
            tool_calls: Some(calls),
            ..assistant
        };
        let a = turn_to_chat(&assistant);
        assert_eq!(a.role, "assistant");
        assert_eq!(a.tool_calls.len(), 1);
        assert_eq!(a.tool_calls[0].fn_name, "recall_memory");

        // 工具结果行：库里 role='assistant' + tool_call_id 非空，应还原成 role='tool'。
        let result = MessageTurn {
            role: "assistant".to_string(),
            content: "数据".to_string(),
            created_at: rfc(Utc::now()),
            tool_calls: None,
            tool_call_id: Some("c1".to_string()),
            tool_name: Some("recall_memory".to_string()),
        };
        let r = turn_to_chat(&result);
        assert_eq!(r.role, "tool");
        assert_eq!(r.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(r.tool_name.as_deref(), Some("recall_memory"));
        assert!(r.tool_calls.is_empty());

        // 普通消息不受影响。
        let plain = turn("user", "在吗", &rfc(Utc::now()));
        let p = turn_to_chat(&plain);
        assert_eq!(p.role, "user");
        assert!(p.tool_calls.is_empty());
        assert!(p.tool_call_id.is_none());
    }
}
