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
///
/// 压缩以工具调用组为单位（见 [`tool_group_end`]），整组同留同弃，故实际条数可能
/// 略超这个上限——最多超一组工具往返。
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
/// - `Some(n)`：退化为取最近 N 条（不做按天整理，同样尊重 `exclude_id`）。
///
/// `exclude_id` 为 `Some(id)` 时，把历史**截断到该消息之前**（重试场景：被复用的用户消息
/// 及其后来产生的半截 assistant/tool 轮次都要排除，令 `messages` 等价于"首轮重跑"——
/// 该消息之前的全部历史 + 末尾再显式 push 一次本条用户消息）。截断点即该消息首次出现处；
/// 历史里没有它（如被日界挡掉）时保持原样，同样由末尾 push 保证恰好出现一次。
///
/// **调用时机**：传了 `exclude_id`（Telegram：消息在通道侧收到时就已落库）就不必挑时机，
/// 截断保证它不会既在历史里、又在末尾被 push 一次；未传时（Web/iLink：由 `chat_stream`
/// 自行入库）必须在入库之前取，否则当前消息会被算进历史、在提示词里出现两遍。
pub(crate) fn fetch(
    conversation: &Sessions,
    session_id: &str,
    history_count: Option<u32>,
    exclude_id: Option<&str>,
) -> Result<Vec<ChatMessage>> {
    match history_count {
        Some(n) => recent_history(conversation, session_id, n as usize, exclude_id),
        None => {
            let start = day_window_start(USER_OFFSET_MINUTES);
            let mut turns = conversation.messages_since(session_id, start)?;
            if turns.len() < DAY_POOL_THRESHOLD {
                turns = conversation.messages_since(session_id, start - Duration::days(1))?;
            }
            if let Some(ex) = exclude_id {
                if let Some(pos) = turns.iter().position(|t| t.id == ex) {
                    turns.truncate(pos);
                }
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
    recent_history(conversation, session_id, limit, None)
}

/// 取最近 `limit` 条历史并做配对净化；`exclude_id` 为 `Some` 时截断到该消息之前。
///
/// [`fetch`] 的 `Some(n)` 路径与 [`fetch_recent`] 共用此实现——重试的重跑语义
/// （排除被复用的用户消息及其半截轮次）在两条取法上都成立，不该因宿主传了
/// `history_count` 就失效。
fn recent_history(
    conversation: &Sessions,
    session_id: &str,
    limit: usize,
    exclude_id: Option<&str>,
) -> Result<Vec<ChatMessage>> {
    let mut turns = conversation.recent_messages(session_id, limit)?;
    if let Some(ex) = exclude_id {
        if let Some(pos) = turns.iter().position(|t| t.id == ex) {
            turns.truncate(pos);
        }
    }
    let msgs: Vec<ChatMessage> = turns.iter().map(turn_to_chat).collect();
    Ok(sanitize_tool_pairs(msgs))
}

/// 按"今天"把历史消息构建成对话上下文（参考人的作息与生物节律）：
/// - 入场条数超上限时做骨架压缩（保留开头与结尾、省略中间），控制单轮上下文长度；
/// - 压缩以**工具调用组**为单位（见 [`tool_group_end`]）：协议要求 assistant 的每个工具
///   调用都紧跟配对结果，切在组中间就会产出非法历史，所以整组同留同弃——实际条数因此
///   可能略超上限（最多一组工具往返）；
/// - 说话人靠消息本身的 role 一并传给模型，内容保持原文、不做任何装饰。
pub(crate) fn build_day_history(turns: &[MessageTurn]) -> Result<Vec<ChatMessage>> {
    let total = turns.len();
    let compact = total > DAY_CONTEXT_MAX;
    let tail_start = total.saturating_sub(DAY_CONTEXT_MAX - DAY_KEEP_HEAD);

    let mut keep = vec![true; total];
    if compact {
        for (idx, slot) in keep.iter_mut().enumerate() {
            *slot = idx < DAY_KEEP_HEAD || idx >= tail_start;
        }
        // 整组对齐：组内只要有一条原本被留下，整组都留下（反之整组丢弃）。两个方向都要管——
        // 留下调用声明却丢了它的结果行，和留下结果行却丢了声明，都是 provider 会直接拒的残缺配对。
        let mut idx = 0;
        while idx < total {
            match tool_group_end(turns, idx) {
                Some(end) => {
                    if keep[idx..end].iter().any(|k| *k) {
                        keep[idx..end].fill(true);
                    }
                    idx = end;
                }
                None => idx += 1,
            }
        }
    }

    let out: Vec<ChatMessage> = turns
        .iter()
        .zip(&keep)
        .filter(|(_, kept)| **kept)
        .map(|(m, _)| turn_to_chat(m))
        .collect();
    Ok(sanitize_tool_pairs(out))
}

/// 若 `idx` 处是一条"带工具调用的 assistant 轮"，返回其工具调用组的结束下标（不含）；否则 `None`。
///
/// 组 = 该 assistant 轮 + 紧随其后的连续 `tool` 结果行。协议要求两者成对出现，
/// 因此它是压缩裁剪时不可拆分的最小单位。
fn tool_group_end(turns: &[MessageTurn], idx: usize) -> Option<usize> {
    let head = turns.get(idx)?;
    if head.role != "assistant" || declared_calls(head).is_empty() {
        return None;
    }
    let mut end = idx + 1;
    while end < turns.len() && turns[end].role == "tool" {
        end += 1;
    }
    Some(end)
}

/// 读时净化：丢掉不成对的工具往返片段，保证发给 provider 的消息序列协议合法。
///
/// 库里可能残留残缺配对——落库中途失败（见 `Agent::backfill_tool_rows`）、进程被杀、
/// 或压缩裁剪的边界恰好落在组中间。而 provider 对"有调用没结果"和"有结果没调用"都是直接
/// 400，一条脏行就会让**当天剩余时间的每条请求**都失败，所以读到就修：按 call_id 配对，
/// 结果行找不到声明它的调用就丢弃；调用声明的结果一条都没有，就顺手退掉这条声明
/// （整轮没有正文也没有配对的调用时，这一轮直接不发给模型）。
fn sanitize_tool_pairs(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    // 调用 id → 声明它的那一轮下标（重复声明时以最早的一轮为准）。
    let mut declared_at: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (idx, msg) in messages.iter().enumerate() {
        if msg.role == "assistant" {
            for call in &msg.tool_calls {
                declared_at.entry(call.call_id.clone()).or_insert(idx);
            }
        }
    }

    // 只有"声明之后确实出现了配对结果"的调用才算被回答。
    let mut answered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (idx, msg) in messages.iter().enumerate() {
        if msg.role != "tool" {
            continue;
        }
        let Some(call_id) = msg.tool_call_id.as_ref() else {
            continue;
        };
        if declared_at.get(call_id).is_some_and(|head| *head < idx) {
            answered.insert(call_id.clone());
        }
    }

    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for mut msg in messages {
        if msg.role == "tool" {
            let paired = msg
                .tool_call_id
                .as_ref()
                .is_some_and(|id| answered.contains(id));
            if paired {
                out.push(msg);
            }
            continue;
        }
        if msg.role == "assistant" && !msg.tool_calls.is_empty() {
            msg.tool_calls
                .retain(|call| answered.contains(&call.call_id));
            // 结果全丢了就退化成纯正文轮；连正文都没有（正常：正文走工具）就整轮丢掉。
            if msg.tool_calls.is_empty() && msg.content.trim().is_empty() {
                continue;
            }
        }
        out.push(msg);
    }
    out
}

/// 把一条持久化历史轮次还原成发给模型的 [`ChatMessage`]，拼回工具调用上下文。
///
/// genai 要求 assistant 轮的工具调用必须紧跟对应的 tool 结果。两者都按落库时的原样
/// 读出：助手轮 `role='assistant'` + `tool_calls` JSON，工具结果 `role='tool'` +
/// `tool_call_id` / `tool_name`。
fn turn_to_chat(turn: &MessageTurn) -> ChatMessage {
    let mut msg = ChatMessage::new(turn.role.clone(), turn.content.clone());
    msg.tool_calls = declared_calls(turn);
    msg.tool_call_id = turn.tool_call_id.clone();
    msg.tool_name = turn.tool_name.clone();
    msg
}

/// 解析一轮里落库的工具调用声明（`tool_calls` JSON）；缺失或解析失败视为没有调用。
fn declared_calls(turn: &MessageTurn) -> Vec<ChatCall> {
    turn.tool_calls
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::MessageTurn;
    use chrono::Timelike;

    const EAST8_OFFSET: i64 = 8 * 60;

    fn turn(role: &str, content: &str, created_at: &str) -> MessageTurn {
        MessageTurn {
            id: String::new(),
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

        // 工具结果行：落库时 role 就是 'tool' + tool_call_id，原样还原成 role='tool'。
        let result = MessageTurn {
            id: String::new(),
            role: "tool".to_string(),
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
