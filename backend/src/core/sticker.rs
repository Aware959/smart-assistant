//! 贴纸（表情包）目录：模型可见的**选择键** → Telegram `file_id` 的映射。
//!
//! 两个约束决定了它是一份缓存而不是配置：
//!
//! - **`file_id` 是 bot 专属的**——同一张贴纸在不同 bot、不同 token 下拿到的
//!   `file_id` 互不通用。所以目录只能由当前 bot 自己去 `getStickerSet` 拉，
//!   换了 bot 就得重拉（`file_unique_id` 跨 bot 稳定，但它不能用来发送）。
//! - **模型永远只接触选择键**，不接触 `file_id`。键来自 Telegram 给每张贴纸的
//!   `emoji` 字段（人工看图标注的成本太高），同 emoji 的重复贴纸追加序号区分；
//!   键不贴切时直接手改 JSON 即可，模型侧只认改后的键。
//!
//! 文件缺失或损坏一律按空目录处理：贴纸能力自动关闭，其余功能不受影响。
use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// 贴纸目录。空目录 = 没有贴纸能力（[`crate::llm::tool::definitions`] 不下发该工具）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickerCatalog {
    /// 贴纸包名（Telegram `name`，即 `getStickerSet` 的入参），如 `DzhersiTyan4`。
    #[serde(default)]
    pub set: String,
    /// 贴纸包标题（展示用）。
    #[serde(default)]
    pub title: String,
    /// 最近一次拉取时刻（RFC3339 世界时）。
    #[serde(default)]
    pub updated_at: String,
    /// 选择键 → 该张贴纸的 `file_id`。用 `BTreeMap` 让键的下发顺序稳定。
    #[serde(default)]
    pub stickers: BTreeMap<String, String>,
}

impl StickerCatalog {
    /// 空目录：贴纸能力关闭。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 目录里一张贴纸都没有。
    pub fn is_empty(&self) -> bool {
        self.stickers.is_empty()
    }

    /// 模型可见的选择键（已按序排好）。
    pub fn keys(&self) -> Vec<String> {
        self.stickers.keys().cloned().collect()
    }

    /// 选择键取 `file_id`；键不存在返回 `None`。
    pub fn file_id(&self, key: &str) -> Option<&str> {
        self.stickers.get(key).map(String::as_str)
    }

    /// 由 `getStickerSet` 拉到的一包贴纸生成目录。
    ///
    /// 选择键取每张贴纸的 `emoji`；空 emoji 归到 `•`，同键重复依次加 `2`/`3`……
    /// 入参是有序的 `(emoji, file_id)` 序列，键的生成因此是确定的。
    pub fn from_set(set: &str, title: &str, stickers: &[(String, String)]) -> Self {
        let mut map = BTreeMap::new();
        for (emoji, file_id) in stickers {
            let base = emoji.trim();
            let base = if base.is_empty() { "•" } else { base };
            let mut key = base.to_string();
            let mut n = 2;
            while map.contains_key(&key) {
                key = format!("{base}{n}");
                n += 1;
            }
            map.insert(key, file_id.clone());
        }
        Self {
            set: set.to_string(),
            title: title.to_string(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            stickers: map,
        }
    }

    /// 按配置的文件路径加载。**失败不报错**——目录缺失属正常状态（还没拉过），
    /// 它只决定贴纸工具在不在，不该让整个 Agent 起不来。
    pub fn load(path: &str) -> Self {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(_) => return Self::default(),
        };
        match serde_json::from_str(&raw) {
            Ok(catalog) => catalog,
            Err(e) => {
                tracing::warn!(%path, error = %e, "贴纸目录损坏，按空目录处理（贴纸能力关闭）");
                Self::default()
            }
        }
    }

    /// 按配置的文件路径加载。
    pub fn load_default() -> Self {
        Self::load(&Config::get().sticker_catalog_file)
    }

    /// 覆写写入（pretty JSON，便于手改键后重新拉取时对照）。
    pub fn save(&self, path: &str) -> Result<(), String> {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建目录 {} 失败: {e}", parent.display()))?;
            }
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("序列化贴纸目录失败: {e}"))?;
        std::fs::write(path, json).map_err(|e| format!("写入 {path} 失败: {e}"))
    }
}

#[cfg(test)]
#[path = "sticker_tests.rs"]
mod sticker_tests;
