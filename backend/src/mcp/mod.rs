//! MCP 客户端：让模型能调用本机 MCP 服务器上的工具。
//!
//! 定位：MCP 是「模型调用工具」的通用协议，本仓把它当作**工具目录的延伸**——
//! 内置工具（发消息、贴纸、记忆…）照旧，MCP 服务器提供的工具按
//! `mcp__<服务器>__<工具>` 命名并入同一张工具表，模型用同一套调用方式发起。
//!
//! 支持两种传输：
//!
//! - **stdio**（本地子进程）：覆盖绝大多数自建服务器（ComfyUI 的 `comfy-mcp` 等），
//!   不需要鉴权与会话管理。
//! - **Streamable HTTP**：配置里给 `url` 就走这条。服务器用 `Mcp-Session-Id` 头
//!   维持会话；本机已挂着的网关（如 magpie 的 `fetch`）就是这种形态。
//!
//! OAuth、连接复用、通知推送都没做——按需再加。

pub mod client;
pub mod config;
pub mod protocol;
pub mod registry;

pub use registry::{Artifact, McpOutcome, Target};

/// 工具名前缀。
pub const TOOL_PREFIX: &str = registry::TOOL_PREFIX;

/// 该工具名是否来自 MCP。
pub fn is_mcp_tool(name: &str) -> bool {
    name.starts_with(TOOL_PREFIX)
}

/// 拆出 MCP 工具名里的服务器与工具名；不是 MCP 工具则 `None`。
pub fn split_target(name: &str) -> Option<Target> {
    registry::split_target(name)
}

/// 该工具执行前是否需要用户点头（按 `mcp.json` 的 `confirm` 段 + 全局缺省判定）。
pub fn requires_confirmation(name: &str) -> bool {
    registry::requires_confirmation(name)
}

/// 下发给模型的 MCP 工具定义。服务器不可用时为空。
pub fn tool_definitions() -> Vec<genai::chat::Tool> {
    registry::tool_definitions()
}

/// 执行一次 MCP 工具调用。
pub fn dispatch(name: &str, arguments: &serde_json::Value) -> Result<McpOutcome, String> {
    registry::dispatch(name, arguments)
}

/// 预热：连上服务器、取回工具表。
pub fn warmup() {
    registry::warmup();
}

/// 收尾：关掉所有 MCP 子进程。
pub fn shutdown() {
    registry::shutdown();
}

/// 逐个服务器尝试连接，回报状态与工具列表（运维 CLI 用）。
pub fn server_status() -> Vec<registry::ServerStatus> {
    registry::server_status()
}

/// 配置载入失败的原因。
pub fn load_error() -> Option<String> {
    registry::load_error()
}

/// 按服务器名与工具名调用一次 MCP 工具。
pub fn call(server: &str, tool: &str, arguments: &serde_json::Value) -> Result<McpOutcome, String> {
    registry::dispatch(&format!("{TOOL_PREFIX}{server}__{tool}"), arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tool_names_are_recognized() {
        assert!(is_mcp_tool("mcp__comfy__run_workflow"));
        assert!(!is_mcp_tool("send_message"));
        assert!(!is_mcp_tool("generate_image"));
    }
}
