//! Telegram inbound 子模块。

use super::*;

/// 从一条入站消息里取出交给模型的文本。
///
/// Telegram 把非文本消息放进各自的字段（`sticker` / `photo` / `voice` …），没有
/// `text`。目前只认**贴纸**：转成一句可读的占位描述，让模型知道"对方发来一张
/// 表情包"，而不是整条被丢掉。其余非文本类型暂不处理——返回 None 即忽略。
/// 这跟出站侧 [`Reply::Sticker`] 的投影是对称的。
pub(super) fn inbound_text(message: &TgMessage) -> Option<String> {
    if let Some(text) = message.text.as_deref() {
        if !text.trim().is_empty() {
            return Some(text.to_string());
        }
    }
    let emoji = message
        .sticker
        .as_ref()
        .and_then(|s| s.emoji.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match emoji {
        Some(e) => Some(format!("[对方发来一张表情包 {e}]")),
        None if message.sticker.is_some() => Some("[对方发来一张表情包]".to_string()),
        None => None,
    }
}

/// 与 [`inbound_text`] 同构的"本条消息说了什么"，但把语音/音频也纳进来。
///
/// 拆成"先解析、后干活"两步是为了满足 FR-003：**过期**的语音必须能被识别出来并直接
/// 存档，绝不能先付一遍下载 + 转写的代价才发现它根本不会被回复（见 `run_loop`）。
/// 本条消息的内容来源。生命周期与 [`TgMessage`] 的借用绑定（音频字段是借来的）。
pub(super) enum Inbound<'a> {
    /// 直接可用的文本（含贴纸占位描述）。
    Text(String),
    /// 需要转写的音频附件。
    Audio(AudioRef<'a>),
}
impl TgMessage {
    /// 本条消息的"内容来源"：文本/贴纸优先，其次音频附件，都没有则 `None`（忽略）。
    ///
    /// 与 [`inbound_text`] 的优先级一致：文本 > 贴纸 > 语音 > 音频。
    pub(super) fn resolve_inbound(&self) -> Option<Inbound<'_>> {
        if let Some(text) = inbound_text(self) {
            return Some(Inbound::Text(text));
        }
        // `inbound_text` 对语音/音频返回 None（它们没有 text 也没有 sticker），
        // 所以走到这里才轮到音频附件——顺序天然正确。
        self.audio_attachment().map(Inbound::Audio)
    }
}
