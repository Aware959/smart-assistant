//! 通道文本分片原语。

/// 把长文本按字符上限切分为多段（优先在换行处断开，中文按字符计数）。
/// 各渠道发送接口都有单条长度限制，统一用该函数分片。
pub fn chunk_text(text: &str, limit: usize) -> Vec<String> {
    chunk_by_units(text, limit, |_| 1)
}

/// 把长文本按 **UTF-16 码元**上限切分为多段（优先在换行处断开）。
///
/// 给上限按 UTF-16 计的渠道用（Telegram：一条消息 4096 个 UTF-16 码元，
/// emoji 等非 BMP 字符占 2，按字符数分片会切出超限片段被服务端拒收）。
pub fn chunk_text_utf16(text: &str, limit: usize) -> Vec<String> {
    chunk_by_units(text, limit, char::len_utf16)
}

/// 按“长度单位”上限切分：`units` 给出单个字符占几个单位（字符渠道恒为 1，UTF-16 渠道看字符本身）。
///
/// 切口永远落在字符边界上（按 `char_indices` 走），不会切出半个字符。
fn chunk_by_units<F>(text: &str, limit: usize, units: F) -> Vec<String>
where
    F: Fn(char) -> usize,
{
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
        // 累到“再加一个字符就超限”处的字节下标；整段都没超限时保持 `rest.len()`。
        let mut end = rest.len();
        let mut used = 0usize;
        for (i, c) in rest.char_indices() {
            used += units(c);
            if used > limit {
                end = i;
                break;
            }
        }
        if end == 0 {
            // 单个字符就超过上限（如上限 1 遇到代理对）：仍切出这一个字符，保证循环前进。
            end = rest
                .char_indices()
                .nth(1)
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
        }
        if end == rest.len() {
            chunks.push(rest.to_string());
            break;
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
