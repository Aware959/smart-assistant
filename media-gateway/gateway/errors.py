"""网关统一错误类型。"""

from __future__ import annotations


class GatewayError(Exception):
    """对外暴露的错误：带 HTTP 状态码 + 给调用方的说明文本。

    通道层（Rust）只看到状态码和 `{"error": detail}`，故 detail 应是可直接回显的短句。
    """

    def __init__(self, status: int, detail: str):
        self.status = status
        self.detail = detail
        super().__init__(detail)
