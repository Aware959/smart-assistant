//! 外部渠道接入：把第三方平台的消息翻译为内部对话流水线调用。
//!
//! 每个渠道只做三件事：收消息 → 调 [`crate::Assistant::chat_stream`] → 回消息。
//! 会话隔离由 [`crate::db::channel`] 负责。

#[cfg(feature = "telegram")]
pub mod telegram;

#[cfg(feature = "ilink")]
pub mod ilink;

/// 把长文本按字符上限切分为多段（优先在换行处断开，中文按字符计数）。
/// 各渠道发送接口都有单条长度限制，统一用该函数分片。
pub fn chunk_text(text: &str, limit: usize) -> Vec<String> {
    if text.is_empty() || limit == 0 {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![text.to_string()]
        };
    }
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.chars().count() <= limit {
            chunks.push(rest.to_string());
            break;
        }
        // 前 limit 个字符的字节结束位置（char_indices 保证落在字符边界）。
        let mut end = rest.len();
        for (i, _) in rest.char_indices() {
            if rest[..i].chars().count() >= limit {
                end = i;
                break;
            }
        }
        // 优先在换行处断开。
        if let Some(nl) = rest[..end].rfind('\n') {
            end = nl + 1;
        }
        chunks.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_short_text_untouched() {
        assert_eq!(chunk_text("hello", 4096), vec!["hello".to_string()]);
        assert!(chunk_text("", 10).is_empty());
    }

    #[test]
    fn chunks_long_text_within_limit() {
        let text = "a".repeat(9000);
        let chunks = chunk_text(&text, 4096);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunks_prefer_newline_and_keep_cjk() {
        // 换行在首段范围内 → 首段应在换行处断开
        let text = format!("{}\n{}", "a".repeat(4000), "b".repeat(4000));
        let chunks = chunk_text(&text, 4096);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), text);
        assert!(chunks[0].ends_with('\n'));

        // 中文按字符计数，不断开字符
        let cjk = "你好".repeat(3000);
        let chunks = chunk_text(&cjk, 4096);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), cjk);
    }
}
