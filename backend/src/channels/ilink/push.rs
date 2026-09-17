//! iLink 主动推送（窗口式）：维护登录会话 + 每个用户的最近 context_token 缓存。
//!
//! iLink 的主动发送是"窗口式"的：用户先发消息拿到 `context_token`，缓存后可在窗口内
//! （约 24h / `ILINK_MAX_SENDS_PER_REFRESH` 次的保守取值）主动发送；超窗或超次数须等
//! 用户再次入站刷新。窗口可用性由 [`super::super::IlinkContact::is_fresh`] 判定。

use super::protocol;
use super::session::SavedSession;
use super::{DEFAULT_BASE};
use super::super::{IlinkContact, IlinkRegistry};
use crate::config::Config;

/// 登录会话更新：登录/重登录后调用（重登录时清空全部用户 token 缓存）。
pub(crate) fn set_session(sess: SavedSession, drop_contacts: bool) {
    let mut reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
    let entry = reg.get_or_insert_with(IlinkRegistry::default);
    entry.sess = Some(sess);
    if drop_contacts {
        entry.contacts.clear();
    }
}

/// 入站消息刷新某用户的 context_token（同时作为"对方刚说话"的证明）。
pub(crate) fn refresh_contact(from_user_id: &str, context_token: &str) {
    if context_token.trim().is_empty() {
        return;
    }
    let mut reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(entry) = reg.as_mut() {
        entry.contacts.insert(
            from_user_id.to_string(),
            IlinkContact {
                context_token: context_token.to_string(),
                observed_at: chrono::Utc::now(),
                sends_since_refresh: 0,
            },
        );
    }
}

/// 主动推送：仅当该用户带新鲜 context_token 时才会真正发送。
/// 返回 Err 表示当前无可用窗口（token 缺失/过期/超次），调用方应跳过该用户。
pub(crate) async fn push_send(external_id: &str, text: &str) -> Result<(), String> {
    let cfg = Config::get();
    let (sess, token) = {
        let reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = reg.as_ref() else {
            return Err("iLink 尚未登录".to_string());
        };
        let Some(sess) = entry.sess.clone() else {
            return Err("iLink 会话为空".to_string());
        };
        let Some(contact) = entry.contacts.get(external_id) else {
            return Err("该用户无 context_token（需先主动发消息）".to_string());
        };
        if !contact.is_fresh(chrono::Utc::now(), cfg.ilink_context_max_age_hours) {
            return Err("context_token 已过窗口，等待用户再次入站刷新".to_string());
        }
        (sess, contact.context_token.clone())
    };

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let base = if sess.baseurl.trim().is_empty() {
        DEFAULT_BASE.to_string()
    } else {
        sess.baseurl.clone()
    };

    match protocol::send_text(&client, &base, &sess, external_id, &token, text).await {
        Ok(()) => {
            // 发送成功：递增窗口计数（iLink 200 不代表 100% 投递，按协议层成功计，
            // 用户下次入站自然会刷新窗口）。
            let mut reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = reg.as_mut() {
                if let Some(c) = entry.contacts.get_mut(external_id) {
                    c.sends_since_refresh += 1;
                }
            }
            Ok(())
        }
        Err(e) => {
            tracing::warn!(
                user = external_id,
                error = %e,
                "iLink 主动推送失败，丢弃该用户 token"
            );
            let mut reg = push_registry().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = reg.as_mut() {
                entry.contacts.remove(external_id);
            }
            Err(e)
        }
    }
}

fn push_registry() -> &'static std::sync::Mutex<Option<IlinkRegistry>> {
    &super::super::shared_push().ilink
}