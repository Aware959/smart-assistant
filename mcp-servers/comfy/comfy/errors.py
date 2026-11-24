"""统一错误类型。

旧媒体网关的错误带 HTTP 状态码（那是它自己造 HTTP 接口的产物）；这里对外是 MCP，
错误只需要一句人读得懂的话——故只保留 detail 与一个机器可读的 kind。
"""

from __future__ import annotations


class ComfyError(Exception):
    """出图过程中的错误。

    `kind` 是可选的机器可读分类（如 `comfy_unreachable`），便于日志与排错时区分
    "服务没起来"和"工作流不对"这两类完全不同的故障。
    """

    def __init__(self, detail: str, kind: str | None = None):
        self.detail = detail
        self.kind = kind
        super().__init__(detail)
