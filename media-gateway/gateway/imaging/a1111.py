"""A1111（StabilityMatrix 前端常见）出图后端。

需后端启动参数带 `--api`，开放 `/sdapi/v1/txt2img`。
"""

from __future__ import annotations

import base64
import json
import urllib.error

from ..config import settings
from ..errors import GatewayError
from ..httpclient import post_json


def generate(req: dict) -> tuple[bytes, str]:
    """把请求转成 A1111 txt2img，返回 `(图片字节, MIME)`。"""
    payload = {
        "prompt": req.get("prompt", ""),
        "negative_prompt": req.get("negative_prompt") or "",
        "width": req.get("width") or 512,
        "height": req.get("height") or 512,
        "steps": req.get("steps") or 20,
        "cfg_scale": req.get("cfg_scale") or 7.0,
        "sampler_name": req.get("sampler_name") or "Euler",
        "n_iter": 1,
        "batch_size": 1,
        "restore_faces": False,
        "enable_hr": False,
        "override_settings": {},
        "override_settings_restore_afterwards": True,
    }
    url = f"{settings.diffusion_backend_url.rstrip('/')}/sdapi/v1/txt2img"
    try:
        status, body = post_json(url, payload)
    except urllib.error.URLError as e:
        raise GatewayError(502, f"无法连接 A1111 {url}: {e}")
    if status != 200:
        raise GatewayError(502, f"A1111 返回 {status}: {body[:500]}")
    data = json.loads(body)
    images = data.get("images") or []
    if not images:
        raise GatewayError(502, f"A1111 未返回图片：{str(data)[:500]}")
    ctype = data.get("content_type", "image/png")
    try:
        raw = base64.b64decode(images[0])
    except Exception as e:  # noqa: BLE001
        raise GatewayError(500, f"图片解码失败：{e}")
    return raw, ctype
