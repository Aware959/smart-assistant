"""ComfyUI 出图后端。

ComfyUI 没有同步出图接口：把工作流 POST 到 `/prompt` 拿 `prompt_id`，轮询 `/history`
直到完成，再从 outputs 取第一张图经 `/view` 拉回字节。
"""

from __future__ import annotations

import json
import random
import time
import urllib.error

from .config import settings
from .errors import ComfyError
from .httpclient import get_bytes, get_json, post_json
from .workflow import apply_prompt, load_workflow, resolve_nodes


def generate(req: dict) -> tuple[bytes, str]:
    """注入 prompt → 提交 /prompt → 轮询 /history → 拉取首张图。"""
    wf = load_workflow()
    nodes = resolve_nodes(wf)
    # 注入正向提示词（模板字符串注入），负向提示词保持工作流原值。
    apply_prompt(wf, nodes, req)
    # 步数 / CFG（可选），并随机种子避免连出同图。
    if nodes.get("ksampler") and nodes["ksampler"] in wf:
        ks = wf[nodes["ksampler"]]["inputs"]
        if req.get("steps"):
            ks["steps"] = req["steps"]
        if req.get("cfg_scale") is not None:
            ks["cfg"] = req["cfg_scale"]
        ks["seed"] = random.randint(0, 2**32 - 1)

    base = settings.comfyui_url.rstrip("/")
    try:
        # 提交工作流。
        status, body = post_json(
            f"{base}/prompt",
            {"prompt": wf, "client_id": f"sa-gateway-{random.randint(0, 1 << 30)}"},
        )
    except urllib.error.URLError as e:
        raise ComfyError(f"无法连接 ComfyUI {base}: {e}")
    if status != 200:
        raise ComfyError(f"ComfyUI /prompt 返回 {status}: {body[:500]}")
    prompt_id = json.loads(body).get("prompt_id")
    if not prompt_id:
        raise ComfyError(f"ComfyUI 未返回 prompt_id：{body[:500]}")

    # 轮询 /history 直到完成（带总超时）。
    deadline = time.time() + settings.timeout
    hist = None
    while time.time() < deadline:
        try:
            s, b = get_json(f"{base}/history/{prompt_id}")
        except urllib.error.URLError:
            s, b = 0, ""
        if s == 200:
            h = json.loads(b)
            if prompt_id in h:
                hist = h
                break
        time.sleep(1.0)
    if hist is None:
        raise ComfyError("ComfyUI 出图超时")

    outputs = hist[prompt_id].get("outputs", {})
    img = None
    for node_out in outputs.values():
        imgs = node_out.get("images") or []
        if imgs:
            img = imgs[0]
            break
    if not img:
        raise ComfyError("ComfyUI 未产出图片")

    # 经 /view 下载实际字节。
    try:
        raw = get_bytes(
            f"{base}/view",
            {
                "filename": img["filename"],
                "subfolder": img.get("subfolder", ""),
                "type": img.get("type", ""),
            },
        )
    except urllib.error.URLError as e:
        raise ComfyError(f"ComfyUI /view 失败：{e}")
    return raw, "image/png"
