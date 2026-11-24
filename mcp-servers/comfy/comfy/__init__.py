"""ComfyUI 出图的 MCP 服务器（stdio 传输，纯标准库）。

把本机 ComfyUI 的出图能力暴露成 MCP 工具，供 smart-assistant 的 MCP 客户端调用：
模型调 `mcp__comfy__generate_image`，产出的 PNG 以 MCP 图片内容块回填，直接投给用户。

**为什么是自建而不是用现成的**：`comfy-mcp-server`（社区包，作者是 ComfyUI 核心开发者）
的依赖链在现代 Python 上连踩四个坑——`mcp>=2` 改名 FastMCP、langchain 全家桶、
Windows 默认 GBK 读不了 UTF-8 模板、pydantic 2.14 生成不了它的 `Image` schema。
本仓库里已有一份验证过的 ComfyUI 调用逻辑（工作流节点自动识别、`/prompt` → 轮询
`/history` → 取 `/view`），把它包成 MCP 协议只需一个薄薄的 JSON-RPC 层。

**零 pip 依赖**：只用 Python 标准库，任何 Python ≥ 3.10 都能直接跑。
"""

__all__ = ["comfyui", "config", "errors", "httpclient", "protocol", "tools", "workflow"]
