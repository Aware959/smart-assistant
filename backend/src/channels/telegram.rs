//! Telegram Bot 接入（长轮询）。
//!
//! - 启动时用 `getMe` 校验 token，并丢弃历史积压 update；
//! - 每个 chat 映射独立 session（经 [`crate::core::session::Sessions`] 的会话映射解析），多轮上下文互不串扰；
//! - 文本超长时按 4096 字符分片发送；
//! - 代理优先级：`TELEGRAM_PROXY` → `HTTPS_PROXY` → `ALL_PROXY`
//!   （api.telegram.org 在部分地区不可直连）。

use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::core::agent::Agent;
use crate::core::identity::service::{
    IdentityService, IdentityVersionView, IdentityView, ProposalView,
};
use crate::core::identity::IdentityFieldKey;
use crate::core::session::{classify_inbound, InboundFreshness, Sessions, TurnTicket};
use crate::{ChatInput, ChatOutput, Reply};

const CHANNEL: &str = "telegram";
/// Telegram 单条消息文本上限（UTF-16 码元，见 [`super::chunk_text_utf16`]）。
const MAX_MESSAGE_LEN: usize = 4096;
/// getUpdates 长轮询超时（秒）。
const POLL_TIMEOUT_SECS: u64 = 50;
/// 网络异常后的退避等待（秒）。
const RETRY_DELAY_SECS: u64 = 5;

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    #[serde(default)]
    message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    #[serde(default)]
    text: Option<String>,
    /// 贴纸消息：没有 `text`，内容在这一段里。目前只取 `emoji` 作可读标签。
    #[serde(default)]
    sticker: Option<TgSticker>,
    /// 语音条：用户按住说话录的，Telegram 侧就是 OGG/OPUS。
    #[serde(default)]
    voice: Option<TgVoice>,
    /// 音频文件：用户以"文件"方式发的录音/音乐（可能是 mp3、m4a…）。
    #[serde(default)]
    audio: Option<TgAudio>,
    chat: TgChat,
    /// 消息发出时刻（Unix 秒，服务端收到即记），等于用户按下发送的世界时刻。
    #[serde(default)]
    date: Option<i64>,
}

/// 入站语音条。**只关心它在不在**：听写能力已下线，收到语音一律回一句"发文字给我"，
/// 不再解析时长/体积/MIME（那些字段只服务于转写的前置拦截）。
#[derive(Debug, Deserialize)]
struct TgVoice {}

/// 入站音频文件（用户以"文件"方式发的录音）。同样只关心在不在。
#[derive(Debug, Deserialize)]
struct TgAudio {}

impl TgMessage {
    /// 本条消息带不带音频（语音条或音频文件）。
    ///
    /// 听写能力已下线，这里只回答"在不在"：通道据此回一句"发文字给我"，
    /// 而不是把整条消息默默丢掉。
    fn has_audio(&self) -> bool {
        self.voice.is_some() || self.audio.is_some()
    }
}

/// 入站贴纸（只取生成可读文本用得上的字段）。
#[derive(Debug, Deserialize)]
struct TgSticker {
    #[serde(default)]
    emoji: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

/// `getStickerSet` 返回的贴纸包（只要生成目录用得上的几个字段）。
#[derive(Debug, Deserialize)]
struct StickerSet {
    #[serde(default)]
    name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    stickers: Vec<StickerItem>,
}

/// 贴纸包里的一张贴纸。`emoji` 是 Telegram 给的标签，这里被当作模型的选择键。
#[derive(Debug, Deserialize)]
struct StickerItem {
    #[serde(default)]
    file_id: String,
    #[serde(default)]
    emoji: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendMessage<'a> {
    chat_id: i64,
    text: &'a str,
}

/// `sendMessage` 响应体。Bot API 会用 200 携带业务错误（`ok:false`），
/// 限流时在 `parameters.retry_after` 里给出应当等待的秒数。
#[derive(Debug, Clone, Default, Deserialize)]
struct SendResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<SendParameters>,
    /// `sendDice` 等业务的 `result` 载荷（骰子点数在 `result.dice.value`）。
    /// 投骰前不知道点数，必须透传出来交给内核，否则模型只能瞎编。
    #[serde(default)]
    result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct SendParameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

mod api;
mod command;
mod delivery;
mod inbound;
mod media;
mod poll;
mod retry;
mod turn;

use api::*;
use command::*;
use delivery::*;
use inbound::*;
use media::*;
use retry::*;
use turn::*;

pub use api::fetch_sticker_set;
pub use poll::run;
pub use retry::run_retry_sweep;

fn push_ready() -> &'static std::sync::atomic::AtomicBool {
    static READY: OnceLock<std::sync::atomic::AtomicBool> = OnceLock::new();
    READY.get_or_init(|| std::sync::atomic::AtomicBool::new(false))
}

/// 主动推送是否可用：token 已配置且长轮询已连通。
pub(crate) fn pushable() -> bool {
    if Config::get().telegram_bot_token.trim().is_empty() {
        return false;
    }
    push_ready().load(std::sync::atomic::Ordering::Relaxed)
}

/// 按 Telegram 外部 id 主动发送文本；外部 id 必须是数字 chat_id。
pub(crate) async fn push_text(external_id: &str, text: &str) -> Result<(), String> {
    let chat_id = external_id
        .parse::<i64>()
        .map_err(|_| "telegram 外部 id 非数字".to_string())?;
    api::push_send(chat_id, text).await
}

#[cfg(test)]
#[path = "telegram_tests.rs"]
mod telegram_tests;
