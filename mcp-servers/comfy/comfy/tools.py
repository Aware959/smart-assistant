"""工具表：暴露给模型的 MCP 工具。

两个工具：

- `generate_image`：出图。产出以 **MCP 图片内容块**回传（base64 PNG），客户端
  会把它变成"投给用户的图片"——不是一条让模型自己念出来的链接。
- `server_info`：ComfyUI 的状态（版本、GPU、当前工作流与节点 id），排错时先用它。

工具执行失败**不是**协议错误：它以 `isError: true` 的正常回应返回，让模型看得见
失败原因并决定下一步（改参数重试、告诉用户、还是放弃）。只有协议层的问题
（未知方法之类）才记成 JSON-RPC 错误。
"""

from __future__ import annotations

import base64
import time

from .comfyui import generate
from .config import settings
from .errors import ComfyError
from .httpclient import get_json
from .protocol import server_info

TOOLS = [
    {
        "name": "generate_image",
        "description": (
            "用本机 ComfyUI 按描述生成一张图片，并把图片直接发给用户。"
            "prompt 请用**英文**描述画面（画面主体、风格、光线、镜头），"
            "扩散模型的提示词是英文的；中文描述可以先用英文写清楚再调用。"
            "生成需要几十秒，请只调用一次，不要反复重试。"
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "画面描述（英文），例如 'a girl with silver hair under neon signs at night, rain'",
                },
                "negative_prompt": {
                    "type": "string",
                    "description": "不希望出现的元素（英文，可选）",
                },
                "width": {"type": "integer", "description": "图宽（像素，可选）"},
                "height": {"type": "integer", "description": "图高（像素，可选）"},
                "steps": {"type": "integer", "description": "采样步数（可选，默认沿用工作流）"},
                "cfg_scale": {"type": "number", "description": "提示词强度（可选，默认沿用工作流）"},
            },
            "required": ["prompt"],
        },
    },
    {
        "name": "server_info",
        "description": "查看本机 ComfyUI 的状态：版本、GPU、当前使用的工作流与节点 id。排错时先用它。",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


def list_tools() -> dict:
    """`tools/list` 的回应。"""
    return {"tools": TOOLS}


def call_generate_image(arguments: dict) -> dict:
    """出图并把 PNG 以图片内容块回传。"""
    prompt = str(arguments.get("prompt") or "").strip()
    if not prompt:
        raise ComfyError("prompt 不能为空：要出图就得说清楚画什么")
    req = {
        "prompt": prompt,
        "negative_prompt": arguments.get("negative_prompt"),
        "width": arguments.get("width"),
        "height": arguments.get("height"),
        "steps": arguments.get("steps"),
        "cfg_scale": arguments.get("cfg_scale"),
    }
    started = time.time()
    raw, mime = generate(req)
    elapsed = time.time() - started
    text = f"已出图（{len(raw)} 字节，耗时 {elapsed:.1f}s）。"
    return {
        "content": [
            {"type": "text", "text": text},
            {
                "type": "image",
                "mimeType": mime,
                "data": base64.b64encode(raw).decode("ascii"),
            },
        ],
        "isError": False,
    }


def call_server_info(_arguments: dict) -> dict:
    """回报 ComfyUI 的状态。查不到就如实说查不到，不编。"""
    lines = [f"ComfyUI 地址：{settings.comfyui_url}"]
    lines.append(f"工作流文件：{settings.comfyui_workflow_file or '（内置兜底工作流）'}")
    lines.append(
        "节点 id 覆盖："
        f"prompt={settings.comfyui_prompt_node} negative={settings.comfyui_negative_node} "
        f"size={settings.comfyui_size_node} ksampler={settings.comfyui_ksampler_node}"
    )
    try:
        status, body = get_json(f"{settings.comfyui_url}/system_stats", timeout=10)
        if status == 200:
            import json as _json

            stats = _json.loads(body)
            system = stats.get("system", {})
            lines.append(f"ComfyUI 版本：{system.get('comfyui_version', '未知')}")
            for device in stats.get("devices", []):
                lines.append(f"GPU：{device.get('name', '未知')}")
        else:
            lines.append(f"ComfyUI 状态查询返回 {status}")
    except Exception as e:  # noqa: BLE001
        lines.append(f"ComfyUI 状态查询失败：{e}")
    return {"content": [{"type": "text", "text": "\n".join(lines)}], "isError": False}


def call_tool(name: str, arguments: dict) -> dict:
    """按名字调用一个工具。失败以 `isError: true` 返回（让模型看得见原因）。"""
    try:
        if name == "generate_image":
            return call_generate_image(arguments)
        if name == "server_info":
            return call_server_info(arguments)
        return {
            "content": [{"type": "text", "text": f"没有名为 `{name}` 的工具"}],
            "isError": True,
        }
    except ComfyError as e:
        return {"content": [{"type": "text", "text": e.detail}], "isError": True}
    except Exception as e:  # noqa: BLE001
        return {
            "content": [{"type": "text", "text": f"工具执行出错（{type(e).__name__}）：{e}"}],
            "isError": True,
        }


def dispatch(method: str, params: dict) -> dict:
    """协议方法分发。"""
    if method == "initialize":
        return server_info()
    if method == "ping":
        return {}
    if method == "tools/list":
        return list_tools()
    if method == "tools/call":
        return call_tool(
            str(params.get("name") or ""),
            params.get("arguments") or {},
        )
    raise KeyError(f"不支持的方法：{method}")
