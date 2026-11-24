# ComfyUI 出图 MCP 服务器

把本机 ComfyUI 的出图能力暴露成 MCP 工具，供 smart-assistant 的 MCP 客户端调用：
模型调 `mcp__comfy__generate_image`，产出的 PNG 以 **MCP 图片内容块**回传，后端直接
投给用户（不经过任何网关进程，也不需要 ffmpeg）。

**零 pip 依赖**：只用 Python 标准库，任何 Python ≥ 3.10 都能直接跑；也可以用 ComfyUI
自带的解释器（`ComfyUI/standalone-env/python.exe`），不必另装环境。

## 为什么是自建

社区包 `comfy-mcp-server`（作者是 ComfyUI 核心开发者）功能对路，但它的依赖链在
现代 Python 上连踩四个坑：`mcp>=2` 把 FastMCP 改名成 MCPServer、langchain 全家桶、
Windows 默认 GBK 读不了 UTF-8 模板、pydantic 2.14 生成不了它的 `Image` schema。
本仓库本来就有一份验证过的 ComfyUI 调用逻辑（工作流节点自动识别、`/prompt` → 轮询
`/history` → 取 `/view`），把它包成 MCP 协议只需一个薄薄的 JSON-RPC 层。

## 配置

由 MCP 客户端的 `mcp.json` 通过 `env` 段传入（也可在本目录放 `.env` 兜底）：

| 变量 | 默认 | 说明 |
|---|---|---|
| `COMFYUI_URL` | `http://127.0.0.1:8188` | ComfyUI API 基址 |
| `COMFYUI_WORKFLOW_FILE` | 空 | API 格式工作流 JSON；相对路径按本目录解析。留空用内置兜底工作流 |
| `COMFYUI_PROMPT_NODE` | `3` | 正向提示词节点 id 覆盖 |
| `COMFYUI_NEGATIVE_NODE` | `4` | 负向提示词节点 id 覆盖 |
| `COMFYUI_SIZE_NODE` | `6` | 尺寸节点 id 覆盖 |
| `COMFYUI_KSAMPLER_NODE` | `5` | 采样器节点 id 覆盖 |
| `COMFYUI_TIMEOUT` | `300` | 单次出图总超时（秒） |

节点 id 只是**可选覆盖**：类型对不上时按 `class_type` 自动识别兜底，换一份工作流
通常不用改这里。

`PYTHONUTF8=1` 建议一并带上（UTF-8 模式）：中文提示词与中文路径都按 UTF-8 处理。

## 工具

- `generate_image(prompt, negative_prompt?, width?, height?, steps?, cfg_scale?)` —
  出图。回传一张图片内容块 + 一句说明（字节数、耗时）。提示词请用英文。
- `server_info()` — ComfyUI 版本、GPU、当前工作流与节点 id。排错先用它。

## 手工跑

```powershell
cd mcp-servers/comfy
python main.py            # stdio 服务器：等 JSON-RPC 输入，Ctrl-C 退出
python -m unittest discover -s tests -v
```

## 自查

```powershell
cd backend
$env:MCP_ENABLED = '1'; $env:MCP_CONFIG_FILE = 'mcp.json'
cargo run --bin zhiwei-cli -- mcp list
cargo run --bin zhiwei-cli -- mcp call comfy generate_image '{"prompt":"a cat on a windowsill at sunset"}'
```
