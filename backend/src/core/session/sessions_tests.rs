//! 运行中轮次的登记与插话游标（"agent 跑着也能插话"的会话侧一半）。

use super::*;

fn sessions() -> Sessions {
    Sessions::new(Arc::new(Mutex::new(Database::in_memory().unwrap()))).unwrap()
}

/// 同一会话同时只允许一轮：第二轮登记失败，第一轮结束后才放行。
#[test]
fn 同一会话同时只跑一轮() {
    let sessions = sessions();
    let sid = sessions.resolve("telegram", "100").unwrap();

    let first = sessions.begin_turn(&sid);
    assert!(first.is_some(), "空闲会话应能登记");
    assert!(
        sessions.begin_turn(&sid).is_none(),
        "已有轮次在跑时不得再起一轮"
    );

    // 别的会话不受影响（不同用户各跑各的）。
    let other = sessions.resolve("telegram", "200").unwrap();
    assert!(sessions.begin_turn(&other).is_some());

    // 轮次结束（凭证丢弃）即放行。
    drop(first);
    assert!(sessions.begin_turn(&sid).is_some());
}

/// 销号认令牌号：旧轮次的凭证晚一步丢弃时，不能把接手的新轮次的号销掉。
#[test]
fn 过期凭证不销新一轮的号() {
    let sessions = sessions();
    let sid = sessions.resolve("telegram", "100").unwrap();

    let stale = sessions.begin_turn(&sid).unwrap();
    // 模拟"旧轮次被强制注销、新轮次接手"：摘掉登记，让新一轮拿到新号。
    sessions.inner.lock().unwrap().turns.remove(&sid);
    let fresh = sessions.begin_turn(&sid).unwrap();

    // 旧轮次的阻塞线程这时才收尾。
    drop(stale);
    assert!(
        sessions.inner.lock().unwrap().turns.contains_key(&sid),
        "旧凭证不得销掉新轮次的号"
    );

    drop(fresh);
    assert!(!sessions.inner.lock().unwrap().turns.contains_key(&sid));
}

/// 插话游标只认水位线之后的**用户**消息：本轮自己的往返（assistant/tool）不进上下文。
#[test]
fn 只取水位线之后的用户消息() {
    let sessions = sessions();
    let sid = sessions.resolve("telegram", "100").unwrap();
    let trigger = sessions.add_message(&sid, "user", "第一条").unwrap();
    // 本轮自己的往返：assistant 调用行 + tool 结果行 + 投递文本行。
    sessions
        .add_message_full(
            &sid,
            "assistant",
            "",
            &crate::db::message::ToolMeta {
                tool_calls: Some(
                    serde_json::to_string(&vec![crate::core::schema::message::ChatCall::new(
                        "c1",
                        "send_message",
                        serde_json::json!({}),
                    )])
                    .unwrap(),
                ),
                tool_call_id: None,
                tool_name: None,
            },
        )
        .unwrap();
    sessions
        .add_message_full(
            &sid,
            "tool",
            "已发送",
            &crate::db::message::ToolMeta {
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
                tool_name: Some("send_message".to_string()),
            },
        )
        .unwrap();
    sessions.add_message(&sid, "assistant", "先回一句").unwrap();
    // 运行中收到的两条插话。
    sessions.add_message(&sid, "user", "在吗").unwrap();
    let third = sessions.add_message(&sid, "user", "顺便问下").unwrap();

    let fresh = sessions.user_messages_after(&sid, &trigger.id).unwrap();
    assert_eq!(
        fresh.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
        vec!["在吗", "顺便问下"],
        "只并用户消息，且按时间正序"
    );

    // 游标推进到最后一条后，再取为空（同一条插话不会并两次）。
    assert!(sessions
        .user_messages_after(&sid, &third.id)
        .unwrap()
        .is_empty());
    // 水位线认不出来时返回空，绝不把整段历史重灌一遍。
    assert!(sessions
        .user_messages_after(&sid, "不存在的 id")
        .unwrap()
        .is_empty());
}

/// 进程重启后某会话的第一条消息：缓冲里不得出现两份，否则插话游标会原地打转。
///
/// 复现路径：`Sessions::new` 只载入 channel_map、`hot` 是空的；重启后的第一条消息经
/// `resolve()` 命中映射直接返回（不水合），随后 `add_message` 先写库再水合，水合快照
/// 就带上了刚插入的那条，再 append 一次即重复。重复的后果是
/// [`Sessions::user_messages_after`] 在每一轮迭代都把同一条当成"新插话"并进上下文，
/// 模型因此以为用户把同一句话发了好几遍。
#[test]
fn 重启后首条消息不会在缓冲里存两份() {
    let db = Arc::new(Mutex::new(Database::in_memory().unwrap()));

    let before = Sessions::new(db.clone()).unwrap();
    let sid = before.resolve("telegram", "100").unwrap();
    before.add_message(&sid, "user", "重启前的一句话").unwrap();
    drop(before);

    // 模拟进程重启：channel_map 从库全量载入，hot 为空。
    let after = Sessions::new(db).unwrap();
    let resumed = after.resolve("telegram", "100").unwrap();
    assert_eq!(resumed, sid, "重启后应复用原来的会话");

    let first = after.add_message(&resumed, "user", "下班啦  宝宝").unwrap();
    assert!(
        after
            .user_messages_after(&resumed, &first.id)
            .unwrap()
            .is_empty(),
        "同一条消息不得在缓冲里存两份（否则每轮迭代都会当成新插话重复并入）"
    );

    // 重启后正常到达的插话仍能取到，且只取一次。
    let steer = after.add_message(&resumed, "user", "在吗").unwrap();
    let fresh = after.user_messages_after(&resumed, &first.id).unwrap();
    assert_eq!(
        fresh.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
        vec!["在吗"]
    );
    assert!(after
        .user_messages_after(&resumed, &steer.id)
        .unwrap()
        .is_empty());
}

/// 失败轮次的重跑目标：比触发消息更新的最后一条用户消息（没有则 None）。
#[test]
fn 重跑目标取更新的最后一条用户消息() {
    let sessions = sessions();
    let sid = sessions.resolve("telegram", "100").unwrap();
    let trigger = sessions.add_message(&sid, "user", "第一条").unwrap();
    assert!(sessions
        .newer_user_message(&sid, &trigger.id)
        .unwrap()
        .is_none());

    sessions.add_message(&sid, "assistant", "回一半").unwrap();
    sessions.add_message(&sid, "user", "插一句").unwrap();
    let latest = sessions.add_message(&sid, "user", "再插一句").unwrap();

    let target = sessions.newer_user_message(&sid, &trigger.id).unwrap();
    assert_eq!(target.map(|m| m.id), Some(latest.id.clone()));
    // 最新一条就是触发消息时回落成 None，由调用方沿用本轮自己的入参。
    assert!(sessions
        .newer_user_message(&sid, &latest.id)
        .unwrap()
        .is_none());
}
