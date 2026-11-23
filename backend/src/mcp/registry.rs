//! MCP 注册表：把多个服务器的工具汇成一张可下发给模型的工具表，并负责路由调用。
//!
//! 模型看到的工具名一律是 `mcp__<服务器>__<工具>`：既避免与内置工具重名，
//! 也让模型在工具名里就看得出"这是外部服务器来的能力"。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use genai::chat::Tool;
use serde_json::Value;

use super::client::McpClient;
use super::config::{self, ServerConfig};
use super::protocol::{CallResult, Content, ToolInfo};

/// 工具名前缀。内置工具不会有这个前缀，故两边不会撞名。
pub const TOOL_PREFIX: &str = "mcp__";

/// 连接失败后的冷却时长：冷却期内不再重连。
///
/// 没有冷却的话，每一轮的工具表都会去重连一遍坏掉的服务器——一次失败的 spawn
/// 要几百毫秒，积在每轮的关键路径上，足够让对话明显变慢。
const RETRY_COOLDOWN: Duration = Duration::from_secs(60);

/// 产物：服务器返回的图片 / 音频 / 文件。
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub filename: Option<String>,
}

/// 一次 MCP 调用的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct McpOutcome {
    /// 回填给模型的文本。
    pub text: String,
    /// 需要投给用户的产物（图片 / 音频 / 文件）。
    pub artifacts: Vec<Artifact>,
}

/// 单个服务器的运行状态。
#[derive(Default)]
struct ServerState {
    client: Option<Arc<McpClient>>,
    /// 缓存的工具表：连上时取一次，之后复用。
    tools: Vec<ToolInfo>,
    /// 连接失败后的冷却截止时间。
    retry_after: Option<Instant>,
    /// 最近一次连接 / 列举失败的原因（供状态查询展示）。
    last_error: Option<String>,
}

/// 一个 MCP 服务器。
struct ServerSlot {
    name: String,
    config: ServerConfig,
    state: Mutex<ServerState>,
}

impl ServerSlot {
    /// 该服务器的这个工具是否需要用户点头才执行。
    ///
    /// 取值链：**逐工具覆盖 > 逐服务器缺省 > 全局缺省**。
    fn confirmation_required(&self, tool: &str) -> bool {
        self.config
            .confirm
            .required(tool, crate::config::Config::get().mcp_confirm_default)
    }
}

/// 一个服务器的连接状态（状态查询与运维 CLI 展示用）。
#[derive(Debug, Clone)]
pub struct ServerStatus {
    pub name: String,
    /// 展示用的目标：HTTP 是 URL，stdio 是命令。
    pub target: String,
    pub connected: bool,
    pub tools: Vec<ToolInfo>,
    pub error: Option<String>,
    /// 需要用户点头才能执行的工具（人工确认闸门点名的那些）。
    pub confirm_tools: Vec<String>,
}

/// 全部 MCP 服务器。
pub struct Registry {
    servers: Vec<Arc<ServerSlot>>,
    /// 配置载入失败的原因（没配 / 文件坏了）。供状态查询说明"为什么一个都没有"。
    load_error: Option<String>,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// 进程级注册表（懒加载：第一次用到 MCP 时才读配置、才连服务器）。
fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::load)
}

impl Registry {
    /// 从配置载入服务器清单。
    fn load() -> Registry {
        let cfg = crate::config::Config::get();
        if !cfg.mcp_enabled {
            return Registry {
                servers: Vec::new(),
                load_error: None,
            };
        }
        let path = Path::new(&cfg.mcp_config_file);
        let file = match config::load_file(path) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(error = %error, "MCP 配置不可用，本次不接入 MCP 服务器");
                return Registry {
                    servers: Vec::new(),
                    load_error: Some(error),
                };
            }
        };
        let servers: Vec<Arc<ServerSlot>> = file
            .mcp_servers
            .into_iter()
            .filter(|(_, config)| !config.disabled)
            .map(|(name, config)| {
                Arc::new(ServerSlot {
                    name,
                    config,
                    state: Mutex::new(ServerState::default()),
                })
            })
            .collect();
        tracing::info!(
            servers = servers.len(),
            config = %cfg.mcp_config_file,
            "MCP 服务器配置已载入"
        );
        Registry {
            servers,
            load_error: None,
        }
    }

    /// 该工具是否需要用户点头才执行（按 `mcp.json` 的 `confirm` 段 + 全局缺省判定）。
    fn requires_confirmation(&self, name: &str) -> bool {
        let Some(target) = split_target(name) else {
            return false;
        };
        self.servers
            .iter()
            .find(|slot| slot.name == target.server)
            .map(|slot| slot.confirmation_required(&target.tool))
            .unwrap_or(false)
    }
}

/// 下发给模型的 MCP 工具定义。
///
/// 服务器不可用时它只是缺席——与内置工具的处置一致：模型看不到一个必然失败的
/// 调用，比看到它然后失败要好。
pub fn tool_definitions() -> Vec<Tool> {
    let mut defs = Vec::new();
    for slot in &registry().servers {
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        for tool in tools_locked(&mut state, slot) {
            if !slot.config.exposes(&tool.name) {
                continue;
            }
            defs.push(
                Tool::new(format!("{TOOL_PREFIX}{}__{}", slot.name, tool.name))
                    .with_description(description_for(
                        &slot.name,
                        &tool,
                        slot.confirmation_required(&tool.name),
                    ))
                    .with_schema(tool.input_schema.clone()),
            );
        }
    }
    defs
}

/// 工具描述：前缀标出服务器来源，模型据此知道这是"外部能力"。
fn description_for(server: &str, tool: &ToolInfo, needs_confirmation: bool) -> String {
    let description = tool.description.trim();
    let base = if description.is_empty() {
        format!(
            "MCP 服务器 `{server}` 提供的工具 `{}`（服务器未给说明）。",
            tool.name
        )
    } else {
        format!("[MCP:{server}] {description}")
    };
    // 要确认的工具在描述里点明：模型知道"这次要等用户点头"，才不会以为调用失败。
    if needs_confirmation {
        format!("{base}（执行前需要用户确认）")
    } else {
        base
    }
}

/// 一次调用的归属：哪个服务器、哪个工具。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub server: String,
    pub tool: String,
}

/// 拆出工具名里的服务器与工具名；不是 MCP 工具则 `None`。
pub fn split_target(name: &str) -> Option<Target> {
    let rest = name.strip_prefix(TOOL_PREFIX)?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some(Target {
        server: server.to_string(),
        tool: tool.to_string(),
    })
}

/// 该工具是否需要用户点头才执行。
///
/// 服务器不存在 / 不是 MCP 工具时一律 `false`：那种调用本来就会失败，
/// 不值得为它去打扰用户。
pub fn requires_confirmation(name: &str) -> bool {
    registry().requires_confirmation(name)
}

/// 取服务器的工具表（必要时连上服务器）。
fn tools_locked(state: &mut ServerState, slot: &ServerSlot) -> Vec<ToolInfo> {
    if let Some(client) = &state.client {
        if client.is_alive() && !state.tools.is_empty() {
            return state.tools.clone();
        }
        if !client.is_alive() {
            tracing::warn!(server = %slot.name, "MCP 服务器进程已退出，准备重连");
            state.client = None;
            state.tools.clear();
        }
    }
    if let Some(until) = state.retry_after {
        if Instant::now() < until {
            return Vec::new();
        }
    }
    let client = match ensure_client(state, slot) {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(server = %slot.name, error = %error, "MCP 服务器连接失败");
            state.last_error = Some(error);
            state.retry_after = Some(Instant::now() + RETRY_COOLDOWN);
            return Vec::new();
        }
    };
    match client.list_tools() {
        Ok(tools) => {
            tracing::info!(server = %slot.name, tools = tools.len(), "MCP 服务器已连接");
            state.tools = tools.clone();
            state.retry_after = None;
            state.last_error = None;
            tools
        }
        Err(error) => {
            tracing::warn!(server = %slot.name, error = %error, "MCP 服务器工具列表获取失败");
            state.last_error = Some(error);
            state.retry_after = Some(Instant::now() + RETRY_COOLDOWN);
            Vec::new()
        }
    }
}

/// 确保服务器已连接。调用方须持有 `state` 的锁。
fn ensure_client(state: &mut ServerState, slot: &ServerSlot) -> Result<Arc<McpClient>, String> {
    if let Some(client) = &state.client {
        if client.is_alive() {
            return Ok(client.clone());
        }
        state.client = None;
    }
    let client = Arc::new(McpClient::connect(&slot.config)?);
    state.client = Some(client.clone());
    Ok(client)
}

/// 执行一次 MCP 工具调用。
pub fn dispatch(name: &str, arguments: &Value) -> Result<McpOutcome, String> {
    let rest = name
        .strip_prefix(TOOL_PREFIX)
        .ok_or_else(|| format!("{name} 不是 MCP 工具"))?;
    let (server, tool) = rest
        .split_once("__")
        .ok_or_else(|| format!("MCP 工具名格式应为 mcp__<服务器>__<工具>，实际是 {name}"))?;
    let slot = registry()
        .servers
        .iter()
        .find(|slot| slot.name == server)
        .ok_or_else(|| format!("没有名为 `{server}` 的 MCP 服务器"))?;
    let client = {
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        match ensure_client(&mut state, slot) {
            Ok(client) => client,
            Err(error) => {
                state.last_error = Some(error.clone());
                state.retry_after = Some(Instant::now() + RETRY_COOLDOWN);
                return Err(error);
            }
        }
    };
    let result = client.call_tool(tool, arguments.clone(), slot.config.timeout())?;
    Ok(build_outcome(result))
}

/// 把协议层的内容块拆成「给模型的文本」与「给用户的产物」。
fn build_outcome(result: CallResult) -> McpOutcome {
    build_outcome_with_limit(result, crate::config::Config::get().mcp_result_max_chars)
}

/// 同上，但结果文本按 `limit`（字符）封顶。
///
/// 上限是**给上下文的**，不是为了好看：MCP 服务器常把整个 JSON / 整份日志塞回来，
/// 原样回填会把这一轮的上下文顶爆，后面的话全进不去。截断后必须**注明截了多少**——
/// 模型得知道它看到的是残片，否则会拿残片当完整结论去回答用户。
fn build_outcome_with_limit(result: CallResult, limit: usize) -> McpOutcome {
    let mut texts: Vec<String> = Vec::new();
    let mut artifacts: Vec<Artifact> = Vec::new();
    for content in result.content {
        match content {
            Content::Text(text) => texts.push(text),
            Content::Binary { mime, data } => {
                if data.is_empty() {
                    tracing::warn!("MCP 返回了空的二进制内容，已跳过");
                    continue;
                }
                artifacts.push(Artifact {
                    bytes: data,
                    mime,
                    filename: None,
                });
            }
            Content::Resource {
                uri,
                mime,
                text,
                blob,
            } => {
                if let Some(text) = text {
                    texts.push(format!("[{uri}]\n{text}"));
                } else if let Some(blob) = blob {
                    if blob.is_empty() {
                        tracing::warn!(uri = %uri, "MCP 返回了空的内嵌资源，已跳过");
                        continue;
                    }
                    artifacts.push(Artifact {
                        bytes: blob,
                        mime,
                        filename: Some(filename_from_uri(&uri)),
                    });
                }
            }
            Content::Link { uri, name, mime } => {
                let mime_note = if mime.trim().is_empty() {
                    String::new()
                } else {
                    format!("，类型 {mime}")
                };
                texts.push(format!("资源链接：{name}（{uri}{mime_note}）"));
            }
            Content::Unknown(value) => texts.push(value.to_string()),
        }
    }
    let mut text = texts.join("\n").trim().to_string();
    if result.is_error {
        text = if text.is_empty() {
            "工具返回错误。".to_string()
        } else {
            format!("工具返回错误：{text}")
        };
    }
    let text = truncate(text, limit);
    McpOutcome { text, artifacts }
}

/// 截断超长文本，并注明被截掉了多少。
///
/// 保留**开头**：工具结果的开头通常是结论（"已生成 3 张图"），结尾多是冗余日志。
fn truncate(text: String, limit: usize) -> String {
    let total = text.chars().count();
    if limit == 0 || total <= limit {
        return text;
    }
    let kept: String = text.chars().take(limit).collect();
    format!(
        "{kept}\n…[结果过长，已截断：原始 {total} 字符，保留前 {limit} 字符，截去 {} 字符]",
        total - limit
    )
}

/// 从 URI 里取文件名（取最后一段路径）；取不到就用通用名。
fn filename_from_uri(uri: &str) -> String {
    let tail = uri
        .split(['/', '\\'])
        .rfind(|part| !part.is_empty())
        .unwrap_or("file");
    // 去掉查询串与片段；末尾的 `:` 也一并去掉（"file://" 这类只有 scheme 的
    // URI 切出来会剩一个 "file:"）。
    let tail = tail.split(['?', '#', ':']).next().unwrap_or(tail).trim();
    if tail.is_empty() {
        "file".to_string()
    } else {
        tail.to_string()
    }
}

/// 预热：把服务器都连上、把工具表取回来。
///
/// 放在服务启动期做，第一轮对话就不必替连接与握手买单。
pub fn warmup() {
    if !crate::config::Config::get().mcp_enabled {
        return;
    }
    for slot in &registry().servers {
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        let tools = tools_locked(&mut state, slot);
        tracing::info!(server = %slot.name, tools = tools.len(), "MCP 服务器预热完成");
    }
}

/// 收尾：关掉所有 MCP 子进程。
pub fn shutdown() {
    if SHUTDOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    for slot in &registry().servers {
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = state.client.take() {
            client.shutdown();
        }
    }
}

/// 逐个服务器尝试连接，回报状态与工具列表。
///
/// 运维 CLI 用它验证配置是否正确（命令路径、环境变量、服务器是否肯握手）——
/// 不必先把整个助手跑起来才发现"图出不来"。
pub fn server_status() -> Vec<ServerStatus> {
    let mut statuses = Vec::new();
    for slot in &registry().servers {
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        let tools = tools_locked(&mut state, slot);
        let connected = state.client.as_ref().is_some_and(|c| c.is_alive());
        statuses.push(ServerStatus {
            name: slot.name.clone(),
            target: slot.config.target(),
            connected,
            confirm_tools: tools
                .iter()
                .filter(|tool| slot.confirmation_required(&tool.name))
                .map(|tool| tool.name.clone())
                .collect(),
            tools: if connected { tools } else { Vec::new() },
            error: state.last_error.clone(),
        });
    }
    statuses
}

/// 配置载入失败的原因（没配 MCP 是正常的，此时是 `None`）。
pub fn load_error() -> Option<String> {
    registry().load_error.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn tool_name_carries_server_and_tool() {
        let tool = ToolInfo {
            name: "run_workflow".into(),
            description: "跑工作流".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let description = description_for("comfy", &tool, false);
        assert!(description.contains("[MCP:comfy]"));
        assert!(description.contains("跑工作流"));
    }

    #[test]
    fn description_falls_back_when_server_gives_none() {
        let tool = ToolInfo {
            name: "x".into(),
            description: "   ".into(),
            input_schema: serde_json::json!({}),
        };
        let description = description_for("srv", &tool, false);
        assert!(description.contains("srv"));
        assert!(description.contains("x"));
    }

    #[test]
    fn description_marks_tools_that_need_confirmation() {
        let tool = ToolInfo {
            name: "run_workflow".into(),
            description: "跑工作流".into(),
            input_schema: serde_json::json!({}),
        };
        assert!(description_for("comfy", &tool, true).contains("需要用户确认"));
        assert!(!description_for("comfy", &tool, false).contains("需要用户确认"));
    }

    #[test]
    fn text_content_becomes_model_text() {
        let outcome = build_outcome(CallResult {
            content: vec![Content::Text("画好了".into())],
            is_error: false,
        });
        assert_eq!(outcome.text, "画好了");
        assert!(outcome.artifacts.is_empty());
    }

    #[test]
    fn binary_content_becomes_artifact() {
        let outcome = build_outcome(CallResult {
            content: vec![Content::Binary {
                mime: "image/png".into(),
                data: b"PNG".to_vec(),
            }],
            is_error: false,
        });
        assert!(outcome.text.is_empty());
        assert_eq!(outcome.artifacts.len(), 1);
        assert_eq!(outcome.artifacts[0].mime, "image/png");
    }

    #[test]
    fn empty_binary_is_skipped() {
        let outcome = build_outcome(CallResult {
            content: vec![Content::Binary {
                mime: "image/png".into(),
                data: Vec::new(),
            }],
            is_error: false,
        });
        assert!(outcome.artifacts.is_empty());
    }

    #[test]
    fn embedded_resource_blob_becomes_artifact_with_filename() {
        let outcome = build_outcome(CallResult {
            content: vec![Content::Resource {
                uri: "file:///tmp/out/pic.png".into(),
                mime: "image/png".into(),
                text: None,
                blob: Some(b"PNG".to_vec()),
            }],
            is_error: false,
        });
        assert_eq!(outcome.artifacts.len(), 1);
        assert_eq!(outcome.artifacts[0].filename.as_deref(), Some("pic.png"));
    }

    #[test]
    fn error_result_is_marked_for_the_model() {
        let outcome = build_outcome(CallResult {
            content: vec![Content::Text("工作流不存在".into())],
            is_error: true,
        });
        assert!(outcome.text.contains("工具返回错误"));
        assert!(outcome.text.contains("工作流不存在"));
    }

    #[test]
    fn filename_is_taken_from_uri_tail() {
        assert_eq!(filename_from_uri("file:///a/b/c.png"), "c.png");
        assert_eq!(filename_from_uri("https://x/y/z.png?token=1"), "z.png");
        assert_eq!(filename_from_uri("file://"), "file");
    }

    #[test]
    fn 超长结果被截断并注明截了多少() {
        let long = "字".repeat(500);
        let outcome = build_outcome_with_limit(
            CallResult {
                content: vec![Content::Text(long)],
                is_error: false,
            },
            100,
        );
        assert!(outcome.text.contains("已截断"));
        assert!(outcome.text.contains("原始 500 字符"));
        assert!(outcome.text.contains("保留前 100 字符"));
        assert!(outcome.text.contains("截去 400 字符"));
    }

    #[test]
    fn 短结果原样保留() {
        let outcome = build_outcome_with_limit(
            CallResult {
                content: vec![Content::Text("画好了".into())],
                is_error: false,
            },
            100,
        );
        assert_eq!(outcome.text, "画好了");
        assert!(!outcome.text.contains("已截断"));
    }

    #[test]
    fn 截断按字符而非字节() {
        // 汉字占三字节：按字节切会切出半个字。
        let outcome = build_outcome_with_limit(
            CallResult {
                content: vec![Content::Text("字".repeat(10))],
                is_error: false,
            },
            4,
        );
        assert!(outcome.text.starts_with("字字字字"));
    }

    #[test]
    fn 工具名能拆出服务器与工具() {
        let target = split_target("mcp__comfy__run_workflow").unwrap();
        assert_eq!(target.server, "comfy");
        assert_eq!(target.tool, "run_workflow");
        assert!(split_target("send_message").is_none());
        assert!(split_target("mcp__comfy").is_none());
    }

    /// 造一个只含一个服务器的注册表（不连任何服务器）。
    fn registry_with(confirm: super::config::ConfirmationPolicy) -> Registry {
        let config = ServerConfig {
            command: "x".into(),
            args: vec![],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            tools: vec![],
            disabled: false,
            timeout_secs: None,
            confirm,
        };
        Registry {
            servers: vec![Arc::new(ServerSlot {
                name: "comfy".to_string(),
                config,
                state: Mutex::new(ServerState::default()),
            })],
            load_error: None,
        }
    }

    #[test]
    fn 确认判定按逐工具逐服务器逐级解析() {
        // 逐工具覆盖优先于逐服务器缺省。
        let registry = registry_with(super::config::ConfirmationPolicy {
            default: Some(true),
            tools: BTreeMap::from([("server_info".to_string(), false)]),
        });
        assert!(registry.requires_confirmation("mcp__comfy__run_workflow"));
        assert!(!registry.requires_confirmation("mcp__comfy__server_info"));

        // 都没点名时落到全局缺省（测试环境下缺省为 false）。
        let registry = registry_with(super::config::ConfirmationPolicy::default());
        assert!(!registry.requires_confirmation("mcp__comfy__run_workflow"));

        // 服务器不存在 / 不是 MCP 工具：一律不确认。
        assert!(!registry.requires_confirmation("mcp__other__run_workflow"));
        assert!(!registry.requires_confirmation("send_message"));
    }

    /// 配置文件里的 `confirm` 段一路走到"要不要问用户"。
    #[test]
    fn 配置文件里的确认段直接决定要不要问用户() {
        let raw = r#"{
            "mcpServers": {
                "fake": {
                    "command": "x",
                    "confirm": {"default": true, "tools": {"echo": false}}
                }
            }
        }"#;
        let file: config::FileConfig = serde_json::from_str(raw).unwrap();
        let registry = Registry {
            servers: vec![Arc::new(ServerSlot {
                name: "fake".to_string(),
                config: file.mcp_servers["fake"].clone(),
                state: Mutex::new(ServerState::default()),
            })],
            load_error: None,
        };
        assert!(registry.requires_confirmation("mcp__fake__run_thing"));
        assert!(!registry.requires_confirmation("mcp__fake__echo"));
    }
}
