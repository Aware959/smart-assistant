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
