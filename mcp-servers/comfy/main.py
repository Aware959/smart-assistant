"""ComfyUI 出图 MCP 服务器入口（stdio 传输）。

用法（由 MCP 客户端拉起，不需要手动跑）：

    python main.py

配置来自环境变量（见 `comfy/config.py` 顶部），一般由 `mcp.json` 的 `env` 段传入：

    COMFYUI_URL=http://127.0.0.1:8188
    COMFYUI_WORKFLOW_FILE=D:\\...\\SD模板2.json

零 pip 依赖，纯标准库。
"""

from __future__ import annotations

from comfy.protocol import serve
from comfy.tools import dispatch


def main() -> None:
    serve(dispatch)


if __name__ == "__main__":
    main()
