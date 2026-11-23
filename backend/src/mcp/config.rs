//! MCP 服务器配置：来自 `mcp.json`（兼容 Claude Code 的 `mcpServers` 格式）。
//!
//! 复用既有格式是有意的：MCP 的配置片段在生态里已经长成惯例，用户从别处抄一份
//! 过来就能用，不必学本仓的私有格式。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 配置文件根。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FileConfig {
    /// 服务器名 → 定义。名即命名空间（工具全名是 `mcp__<名>__<工具>`）。
    ///
    /// 字段名兼容两种写法：`mcpServers`（Claude Code 等生态的惯例）与
    /// `mcp_servers`（snake_case 本仓风格）——用户抄来的配置不该因为大小写风格
    /// 差一个字段就整份不生效。
    #[serde(default, rename = "mcpServers", alias = "mcp_servers")]
    pub mcp_servers: BTreeMap<String, ServerConfig>,
}

/// 传输方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// 本机子进程，JSON-RPC 走 stdin/stdout。
    Stdio,
    /// Streamable HTTP：POST 到一个端点，服务器用 `Mcp-Session-Id` 维持会话。
    Http,
}

/// 人工确认闸门的策略：哪些工具执行前要先问过用户。
///
/// MCP 工具是「以本机身份做外部动作」——跑工作流、写文件、调接口。模型直接发起
/// 就执行，等于把这类动作的裁量权交给了模型。故本策略把"哪些要点头"变成配置：
/// **逐工具 > 逐服务器 > 全局缺省**，逐级细化。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ConfirmationPolicy {
    /// 该服务器所有工具的缺省取值。缺省（`None`）= 跟随全局缺省
    /// （`MCP_CONFIRM_DEFAULT`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
    /// 逐工具覆盖：`true` = 需要确认，`false` = 免确认。
    ///
    /// 用 `BTreeMap` 而不是列表：这里要能表达"整个服务器都要确认，但某个只读工具除外"，
    /// 列表做不到（只能表达"这些要确认"，不能表达"这些不要确认"）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, bool>,
}

impl ConfirmationPolicy {
    /// 该工具是否需要人工确认。
    pub fn required(&self, tool: &str, global_default: bool) -> bool {
        if let Some(value) = self.tools.get(tool) {
            return *value;
        }
        self.default.unwrap_or(global_default)
    }
}

/// 单个 MCP 服务器定义。
///
/// 两种传输写在一份配置里：**有 `url` 就是 HTTP，没有就是 stdio**。
/// 这样从别处（Claude Code 等）抄来的配置片段不用改格式就能用。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    /// 可执行命令（如 `comfy-mcp`）。HTTP 传输时留空。
    #[serde(default)]
    pub command: String,
    /// 命令参数。
    #[serde(default)]
    pub args: Vec<String>,
    /// 传给子进程的环境变量。
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// HTTP 端点（如 `http://127.0.0.1:3425/mcp/run/fetch`）。留空 = 走 stdio。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// HTTP 请求头（如 `Authorization: Bearer ...`）。仅 HTTP 传输用。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// 只暴露这些工具；留空 = 该服务器的全部工具都暴露。
    ///
    /// 这个白名单是**给提示词省 token** 用的：MCP 服务器动辄几十个工具，
    /// 全量下发会让每轮上下文膨胀、模型选错工具。留空表示"我信这个服务器"。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 整个服务器不接（保留配置但临时关掉）。
    #[serde(default)]
    pub disabled: bool,
    /// 单次工具调用超时（秒）。缺省 120——生图这类慢操作要给足时间。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// 人工确认闸门：哪些工具执行前要先问过用户。
    ///
    /// 缺省（整块留空）= 跟随全局缺省 [`crate::config::Config::mcp_confirm_default`]。
    /// 例：`{"default": true, "tools": {"server_info": false}}` =
    /// 该服务器的工具都要确认，但 `server_info` 免确认。
    #[serde(default)]
    pub confirm: ConfirmationPolicy,
}

impl ServerConfig {
    /// 本服务器用哪种传输。
    pub fn transport(&self) -> TransportKind {
        match &self.url {
            Some(url) if !url.trim().is_empty() => TransportKind::Http,
            _ => TransportKind::Stdio,
        }
    }

    /// 展示用的目标描述：HTTP 看 URL，stdio 看命令。
    pub fn target(&self) -> String {
        match self.transport() {
            TransportKind::Http => self.url.clone().unwrap_or_default(),
            TransportKind::Stdio => self.command.clone(),
        }
    }

    /// 本次调用的超时。
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs.unwrap_or(120))
    }

    /// 该工具是否应当暴露给模型。
    pub fn exposes(&self, tool: &str) -> bool {
        self.tools.is_empty() || self.tools.iter().any(|t| t == tool)
    }
}

/// 读取配置文件。文件不存在时返回空配置（不是错误——没配 MCP 是常态）。
pub fn load_file(path: &Path) -> Result<FileConfig, String> {
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let mut raw = std::fs::read_to_string(path).map_err(|e| format!("读取 MCP 配置失败：{e}"))?;
    // 抹掉 UTF-8 BOM：Windows 上的编辑器（记事本、某些 IDE）保存 JSON 会带上 BOM，
    // 而 serde_json 不认它——症状是"配置明明写了却一个服务器都没有"。
    if let Some(stripped) = raw.strip_prefix('\u{feff}') {
        raw = stripped.to_string();
    }
    serde_json::from_str(&raw).map_err(|e| format!("MCP 配置不是合法 JSON：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_code_style_config() {
        let raw = r#"{
            "mcpServers": {
                "comfy": {
                    "command": "comfy-mcp",
                    "args": ["--flag"],
                    "env": {"COMFY_BIN": "C:/comfy/comfy.exe"},
                    "tools": ["run_workflow", "fetch_outputs"]
                }
            }
        }"#;
        let parsed: FileConfig = serde_json::from_str(raw).unwrap();
        let server = &parsed.mcp_servers["comfy"];
        assert_eq!(server.command, "comfy-mcp");
        assert_eq!(server.args, vec!["--flag".to_string()]);
        assert_eq!(server.env["COMFY_BIN"], "C:/comfy/comfy.exe");
        assert!(server.exposes("run_workflow"));
        assert!(!server.exposes("launch_comfyui"));
    }

    #[test]
    fn parses_http_style_config() {
        let raw = r#"{
            "mcpServers": {
                "fetch": {
                    "url": "http://127.0.0.1:3425/mcp/run/fetch",
                    "headers": {"Authorization": "Bearer x"}
                }
            }
        }"#;
        let parsed: FileConfig = serde_json::from_str(raw).unwrap();
        let server = &parsed.mcp_servers["fetch"];
        assert_eq!(server.transport(), TransportKind::Http);
        assert_eq!(server.target(), "http://127.0.0.1:3425/mcp/run/fetch");
        assert_eq!(server.headers["Authorization"], "Bearer x");
        // HTTP 服务器没有命令，但 command 有默认值，不会反序列化失败。
        assert!(server.command.is_empty());
    }

    #[test]
    fn stdio_is_the_default_transport() {
        let server = ServerConfig {
            command: "x".into(),
            args: vec![],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            tools: vec![],
            disabled: false,
            timeout_secs: None,
            confirm: ConfirmationPolicy::default(),
        };
        assert_eq!(server.transport(), TransportKind::Stdio);
        assert_eq!(server.target(), "x");
    }

    #[test]
    fn blank_url_falls_back_to_stdio() {
        let server = ServerConfig {
            command: "x".into(),
            args: vec![],
            env: BTreeMap::new(),
            url: Some("   ".into()),
            headers: BTreeMap::new(),
            tools: vec![],
            disabled: false,
            timeout_secs: None,
            confirm: ConfirmationPolicy::default(),
        };
        assert_eq!(server.transport(), TransportKind::Stdio);
    }

    #[test]
    fn empty_tool_list_exposes_everything() {
        let server = ServerConfig {
            command: "x".into(),
            args: vec![],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            tools: vec![],
            disabled: false,
            timeout_secs: None,
            confirm: ConfirmationPolicy::default(),
        };
        assert!(server.exposes("任意工具"));
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let cfg = load_file(Path::new("不存在的路径/mcp.json")).unwrap();
        assert!(cfg.mcp_servers.is_empty());
    }

    #[test]
    fn bom_prefix_is_stripped() {
        let dir = std::env::temp_dir().join(format!("zhiwei-mcp-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");
        std::fs::write(
            &path,
            "\u{feff}{\"mcpServers\":{\"a\":{\"command\":\"x\"}}}",
        )
        .unwrap();
        let cfg = load_file(&path).unwrap();
        assert_eq!(cfg.mcp_servers.len(), 1);
        assert_eq!(cfg.mcp_servers["a"].command, "x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeout_falls_back_to_default() {
        let server = ServerConfig {
            command: "x".into(),
            args: vec![],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            tools: vec![],
            disabled: false,
            timeout_secs: Some(30),
            confirm: ConfirmationPolicy::default(),
        };
        assert_eq!(server.timeout().as_secs(), 30);
    }

    #[test]
    fn 确认策略逐工具覆盖逐服务器与全局缺省() {
        let policy = ConfirmationPolicy {
            default: Some(true),
            tools: BTreeMap::from([("server_info".to_string(), false)]),
        };
        // 逐工具覆盖优先。
        assert!(!policy.required("server_info", false));
        // 逐服务器缺省次之。
        assert!(policy.required("run_workflow", false));
        // 都没说就落到全局缺省。
        let empty = ConfirmationPolicy::default();
        assert!(!empty.required("run_workflow", false));
        assert!(empty.required("run_workflow", true));
    }

    #[test]
    fn 确认段从配置里读出来() {
        let raw = r#"{
            "mcpServers": {
                "comfy": {
                    "command": "comfy-mcp",
                    "confirm": {"default": true, "tools": {"server_info": false}}
                }
            }
        }"#;
        let parsed: FileConfig = serde_json::from_str(raw).unwrap();
        let server = &parsed.mcp_servers["comfy"];
        assert_eq!(server.confirm.default, Some(true));
        assert_eq!(server.confirm.tools.get("server_info"), Some(&false));
        assert!(server.confirm.required("run_workflow", false));
        assert!(!server.confirm.required("server_info", true));
    }
}
