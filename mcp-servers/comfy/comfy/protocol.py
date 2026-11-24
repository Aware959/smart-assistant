"""MCP 服务器协议层：JSON-RPC 2.0 over stdio（只做 tools 一族）。

协议形状很薄：**一行一条 JSON-RPC**，请求进、回应出。stdin 收到一行就处理一行，
处理完把回应写回 stdout（同样一行一条 JSON）。

三种消息要分清：

- **请求**（有 `id`）→ 必须回应，`result` 或 `error`；
- **通知**（无 `id`，如 `notifications/initialized`）→ 不回应；
- **工具执行失败** → **不是** JSON-RPC 错误，而是 `result` 里 `isError: true`
  的正常回应——工具失败是业务结果，模型要看得见并据此改口径（见 `tools.py`）。
"""

from __future__ import annotations

import json
import sys
from typing import Any, Callable

# 握手时上报的协议版本。与 MCP 规范一致；服务器通常自行降级到它认识的版本。
PROTOCOL_VERSION = "2025-06-18"


def server_info() -> dict:
    """`initialize` 的回应。"""
    return {
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "comfy-mcp", "version": "1"},
    }


def write_message(payload: dict[str, Any], stream=None) -> None:
    """写一条 JSON-RPC 消息到 stdout（并立刻冲刷——管道是阻塞的）。"""
    stream = stream or sys.stdout
    stream.write(json.dumps(payload, ensure_ascii=False) + "\n")
    stream.flush()


def serve(
    handler: Callable[[str, dict], dict],
    stdin=None,
    stdout=None,
) -> None:
    """读 stdin 直到 EOF，每收到一条请求就交给 `handler` 并写回回应。

    `handler(method, params) -> result` 抛异常时记成 JSON-RPC 内部错误
    （工具执行失败不该走到这里——那属于业务结果，由 `tools.py` 自己转成 isError）。

    `stdin` / `stdout` 可注入，便于测试。
    """
    stdin = stdin or sys.stdin
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            # 非 JSON 行（服务器混进来的调试输出）直接忽略，不能让协议崩掉。
            continue
        if not isinstance(message, dict):
            continue
        method = message.get("method")
        message_id = message.get("id")
        # 通知没有 id，不回应。
        if message_id is None or not isinstance(method, str):
            continue
        try:
            result = handler(method, message.get("params") or {})
        except Exception as e:  # noqa: BLE001
            write_message(
                {
                    "jsonrpc": "2.0",
                    "id": message_id,
                    "error": {"code": -32603, "message": f"{type(e).__name__}: {e}"},
                },
                stdout,
            )
            continue
        write_message({"jsonrpc": "2.0", "id": message_id, "result": result}, stdout)
