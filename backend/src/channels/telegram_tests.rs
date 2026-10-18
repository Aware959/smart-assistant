use super::*;

fn msg(text: Option<&str>, sticker_emoji: Option<Option<&str>>) -> TgMessage {
    TgMessage {
        text: text.map(str::to_string),
        sticker: sticker_emoji.map(|e| TgSticker {
            emoji: e.map(str::to_string),
        }),
        chat: TgChat { id: 1 },
        date: None,
    }
}

#[test]
fn 文本消息原样取出() {
    assert_eq!(
        inbound_text(&msg(Some("在的"), None)).as_deref(),
        Some("在的")
    );
}

#[test]
fn 空文本且无贴纸返回none() {
    assert_eq!(inbound_text(&msg(Some("   "), None)), None);
    assert_eq!(inbound_text(&msg(None, None)), None);
}

#[test]
fn 贴纸消息转成可读占位文本() {
    assert_eq!(
        inbound_text(&msg(None, Some(Some("😀")))).as_deref(),
        Some("[对方发来一张表情包 😀]")
    );
}

/// 贴纸没有 emoji 标签（少见但存在）也要能收到，不能整条丢掉。
#[test]
fn 无emoji的贴纸也能收到() {
    assert_eq!(
        inbound_text(&msg(None, Some(None))).as_deref(),
        Some("[对方发来一张表情包]")
    );
}

/// 同时带文字与贴纸时以文字为准（贴纸只是附件，不覆盖用户真正说的话）。
#[test]
fn 文本优先于贴纸() {
    assert_eq!(
        inbound_text(&msg(Some("哈哈"), Some(Some("😀")))).as_deref(),
        Some("哈哈")
    );
}

/// `sendDice` 响应里 `result.dice.value` 就是服务端定好的点数，必须原样取回。
#[test]
fn 骰子响应取出点数() {
    let result = Some(serde_json::json!({ "dice": { "value": 4, "emoji": "🎲" } }));
    assert_eq!(dice_value(&result), Ok(4));
}

/// 缺字段 / 非数字都不能瞎编，一律当失败上抛（点数由服务端决定，编一个等于骗模型）。
#[test]
fn 骰子响应缺字段报错() {
    assert!(dice_value(&None).is_err());
    assert!(dice_value(&Some(serde_json::json!({}))).is_err());
    assert!(dice_value(&Some(serde_json::json!({ "dice": {} }))).is_err());
    assert!(dice_value(&Some(serde_json::json!({ "dice": { "value": "4" } }))).is_err());
}

/// 越界值（非 1–6）同样拒绝：宁可退回通用确认，也不把非法点数交给模型。
#[test]
fn 骰子响应越界报错() {
    assert!(dice_value(&Some(serde_json::json!({ "dice": { "value": 0 } }))).is_err());
    assert!(dice_value(&Some(serde_json::json!({ "dice": { "value": 7 } }))).is_err());
}

/// 旁白上限按 UTF-16 码元算：1024 个 emoji 已经占满 2048，必须判超限。
#[test]
fn 旁白上限按码元计() {
    assert!(caption_fits(&"a".repeat(MAX_CAPTION_LEN)));
    assert!(!caption_fits(&"a".repeat(MAX_CAPTION_LEN + 1)));
    // 每个 emoji 占 2 个码元：512 个刚好，513 个超限。
    assert!(caption_fits(&"😀".repeat(MAX_CAPTION_LEN / 2)));
    assert!(!caption_fits(&"😀".repeat(MAX_CAPTION_LEN / 2 + 1)));
}

/// 失败轮次的重试开关：一条都没送达才重试（用户什么都没看到），有送达就不整轮重跑。
#[test]
fn 有一条送达就不再整轮重试() {
    assert!(should_retry_after_failure(0));
    assert!(!should_retry_after_failure(1));
    assert!(!should_retry_after_failure(3));
}

/// 投递计数只认成功：失败片段不算"用户看到过"，否则该重试的轮次会被白白放弃。
#[test]
fn 投递计数只统计成功送达的片段() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Delivery>();
    let deliverer = TgDeliver::new(tx);
    // 假工人：第一条回成功、第二条回失败，然后收工。
    let worker = std::thread::spawn(move || {
        let first = rx.blocking_recv().expect("第一条投递");
        first.ack.send(Ok(None)).unwrap();
        let second = rx.blocking_recv().expect("第二条投递");
        second.ack.send(Err("发送失败".to_string())).unwrap();
    });

    assert_eq!(deliverer.delivered_count(), 0);
    assert_eq!(deliverer.deliver(&Reply::Text("在".to_string())), Ok(None));
    assert_eq!(deliverer.delivered_count(), 1);
    assert!(deliverer.deliver(&Reply::Text("哦".to_string())).is_err());
    assert_eq!(deliverer.delivered_count(), 1);

    worker.join().unwrap();
}

/// 投递口已关（工人退出）时上抛错误且不计数：调用方据此走失败分支，而不是以为送达了。
#[test]
fn 投递通道关闭时报错且不计数() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Delivery>();
    let deliverer = TgDeliver::new(tx);
    drop(rx);

    assert!(deliverer.deliver(&Reply::Text("在".to_string())).is_err());
    assert_eq!(deliverer.delivered_count(), 0);
}
