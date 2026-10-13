"""基于 urllib 的极简 HTTP 助手。

urllib 会读进程环境里的 NO_PROXY，故本进程对 127.0.0.1 的调用默认不走系统代理，
无需像 Rust 侧那样显式 `.no_proxy()`。
"""

from __future__ import annotations

import json
import urllib.parse
import urllib.request

from .config import settings


def post_json(url: str, payload: dict, timeout: float | None = None):
    """POST JSON，返回 `(status, body_text)`。

    连接层错误（`URLError`）原样抛出，由调用方转成带语义的 `GatewayError`。
    """
    timeout = settings.http_timeout if timeout is None else timeout
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=data,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.status, r.read().decode("utf-8")


def get_bytes(url: str, params: dict | None = None, timeout: float | None = None) -> bytes:
    """GET 并返回原始字节（可带 query 参数）。"""
    timeout = settings.http_timeout if timeout is None else timeout
    if params:
        url = f"{url}?{urllib.parse.urlencode(params)}"
    req = urllib.request.Request(url, headers={"Accept": "*/*"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def get_json(url: str, timeout: float | None = None):
    """GET 并返回 `(status, body_text)`。"""
    timeout = settings.http_timeout if timeout is None else timeout
    req = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.status, r.read().decode("utf-8")
