//! MCP 协议层：JSON-RPC 2.0 消息与工具 / 内容块的解析。
//!
//! 这里只认「本仓用到的子集」：MCP 的完整协议还包含 prompts / resources / sampling
//! 等能力，本仓只取 tools 一族，故解析层对未知字段一律宽容忽略——服务器多给字段
//! 不该让调用失败，只有**形状对不上**才是错误。
//!
//! 协议细节见 <https://modelcontextprotocol.io>。

use serde_json::Value;

/// 握手时上报给服务器的协议版本。
///
/// 取当前较新的版本号：服务器会在响应里回报它自己支持的版本，两边不一致时
/// 服务端通常自行降级到它认识的版本（本仓只用 tools 一族，降级不影响可用性），
/// 故本地不做严格协商，只把对方回报的版本记进日志备查。
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// 服务器提供的一个工具。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    /// 工具名（服务器内部命名空间内唯一）。
    pub name: String,
    /// 给模型看的说明。缺失时用空串——工具仍然可见，只是描述信息量少。
    pub description: String,
    /// 入参 JSON Schema（协议字段 `inputSchema`）。缺失时用"任意对象"兜底。
    pub input_schema: Value,
}

/// 工具调用结果里的一块内容。
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// 纯文本：回填给模型。
    Text(String),
    /// 图片 / 音频：base64 编码，解码后是字节。
    Binary { mime: String, data: Vec<u8> },
    /// 内嵌资源：文本或二进制二选一。
    Resource {
        uri: String,
        mime: String,
        text: Option<String>,
        blob: Option<Vec<u8>>,
    },
    /// 指向别处资源的链接。本仓不下载，只把链接回填给模型。
    Link {
        uri: String,
        name: String,
        mime: String,
    },
    /// 未识别的内容类型：原样保留 JSON，回填给模型（总比丢掉强）。
    Unknown(Value),
}

/// `tools/call` 的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct CallResult {
    pub content: Vec<Content>,
    /// 服务器标记的「这是错误结果」。与协议层的传输错误区分开：后者是调用失败，
    /// 前者是调用成功但业务上出错（两者都要让模型知道，不该静默吞掉）。
    pub is_error: bool,
}

/// 解析 `tools/list` 的结果，取出工具列表。
pub fn parse_tools(result: &Value) -> Vec<ToolInfo> {
    let Some(list) = result.get("tools").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| {
            let name = item.get("name").and_then(|v| v.as_str())?.to_string();
            let description = item
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let input_schema = item
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            Some(ToolInfo {
                name,
                description,
                input_schema,
            })
        })
        .collect()
}

/// 解析 `tools/call` 的结果。
pub fn parse_call_result(result: &Value) -> CallResult {
    let content = result
        .get("content")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().map(parse_content).collect())
        .unwrap_or_default();
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    CallResult { content, is_error }
}

/// 解析单块内容。
fn parse_content(item: &Value) -> Content {
    let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "text" => Content::Text(
            item.get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        ),
        "image" | "audio" => {
            let mime = item
                .get("mimeType")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let data = item
                .get("data")
                .and_then(|v| v.as_str())
                .and_then(decode_base64)
                .unwrap_or_default();
            Content::Binary { mime, data }
        }
        "resource" => {
            let resource = item.get("resource").cloned().unwrap_or(Value::Null);
            let uri = resource
                .get("uri")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let mime = resource
                .get("mimeType")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let text = resource
                .get("text")
                .and_then(|v| v.as_str())
                .map(String::from);
            let blob = resource
                .get("blob")
                .and_then(|v| v.as_str())
                .and_then(decode_base64);
            Content::Resource {
                uri,
                mime,
                text,
                blob,
            }
        }
        "resource_link" => Content::Link {
            uri: item
                .get("uri")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            name: item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            mime: item
                .get("mimeType")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        other => {
            tracing::debug!(kind = other, "MCP 返回了未知内容类型，原样保留");
            Content::Unknown(item.clone())
        }
    }
}

/// 解码 base64。
///
/// MCP 用标准 base64（带填充）传二进制。解码失败时返回 `None`——调用方据此
/// 把该块降级处理，而不是把坏字节当成有效图片投给用户。
pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input.as_bytes())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tools_reads_name_description_and_schema() {
        let result = serde_json::json!({
            "tools": [
                {
                    "name": "run_workflow",
                    "description": "跑一个工作流",
                    "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}
                }
            ]
        });
        let tools = parse_tools(&result);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "run_workflow");
        assert_eq!(tools[0].description, "跑一个工作流");
        assert_eq!(
            tools[0].input_schema["properties"]["path"]["type"],
            serde_json::json!("string")
        );
    }

    #[test]
    fn parse_tools_skips_malformed_entries() {
        let result = serde_json::json!({
            "tools": [
                {"description": "没有名字，不该进列表"},
                {"name": "ok"}
            ]
        });
        let tools = parse_tools(&result);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "ok");
        // 缺失的 schema 兜底成"任意对象"，而不是让模型看到一个残缺 schema。
        assert_eq!(tools[0].input_schema["type"], serde_json::json!("object"));
    }

    #[test]
    fn parse_call_result_reads_text_and_error_flag() {
        let result = serde_json::json!({
            "content": [{"type": "text", "text": "画好了"}],
            "isError": true
        });
        let parsed = parse_call_result(&result);
        assert!(parsed.is_error);
        assert_eq!(parsed.content, vec![Content::Text("画好了".into())]);
    }

    #[test]
    fn parse_call_result_decodes_binary_content() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"PNGDATA");
        let result = serde_json::json!({
            "content": [{"type": "image", "mimeType": "image/png", "data": encoded}]
        });
        let parsed = parse_call_result(&result);
        assert_eq!(
            parsed.content,
            vec![Content::Binary {
                mime: "image/png".into(),
                data: b"PNGDATA".to_vec()
            }]
        );
    }

    #[test]
    fn parse_call_result_keeps_embedded_resource_text() {
        let result = serde_json::json!({
            "content": [{
                "type": "resource",
                "resource": {"uri": "file:///a.txt", "mimeType": "text/plain", "text": "内容"}
            }]
        });
        let parsed = parse_call_result(&result);
        assert_eq!(
            parsed.content,
            vec![Content::Resource {
                uri: "file:///a.txt".into(),
                mime: "text/plain".into(),
                text: Some("内容".into()),
                blob: None
            }]
        );
    }

    #[test]
    fn decode_base64_rejects_garbage() {
        assert!(decode_base64("不是 base64!!").is_none());
        assert_eq!(decode_base64("aGk=").unwrap(), b"hi");
    }
}
