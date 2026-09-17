//! iLink 登录会话的持久化（扫码凭证存到会话文件，避免每次重启重新扫码）。

use serde::{Deserialize, Serialize};

/// 持久化扫码登录凭证，避免每次重启都重新扫码。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SavedSession {
    #[serde(default)]
    pub(crate) bot_token: String,
    #[serde(default)]
    pub(crate) baseurl: String,
    #[serde(default)]
    pub(crate) ilink_bot_id: String,
    #[serde(default)]
    pub(crate) ilink_user_id: String,
}

pub(crate) fn load(path: &str) -> SavedSession {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn save(path: &str, sess: &SavedSession) -> Result<(), String> {
    let data =
        serde_json::to_string_pretty(sess).map_err(|e| format!("序列化会话失败: {e}"))?;
    std::fs::write(path, data).map_err(|e| format!("写入会话文件失败: {e}"))?;
    Ok(())
}