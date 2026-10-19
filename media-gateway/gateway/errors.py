"""网关统一错误类型。"""

from __future__ import annotations


class GatewayError(Exception):
    """对外暴露的错误：带 HTTP 状态码 + 给调用方的说明文本。

    通道层（Rust）只看到状态码和 `{"error": detail}`，故 detail 应是可直接回显的短句。

    `kind` 是可选的机器可读分类（目前只有 ASR 的 `engine_unavailable` 在用）。它**不是**
    主判据——调用方分流看状态码，kind 只作日志与排错时的补充。
    """

    def __init__(self, status: int, detail: str, kind: str | None = None):
        self.status = status
        self.detail = detail
        self.kind = kind
        super().__init__(detail)
