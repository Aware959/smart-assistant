"""ComfyUI 工作流：内置兜底 + 节点自动识别。

节点识别尽量自动，减少手填：顺着 KSampler 的 positive/negative 找提示词节点，
按 class_type 找尺寸节点；环境变量的 `*_NODE` 仅作可选覆盖。
"""

from __future__ import annotations

import copy
import json

from ..config import settings
from ..errors import GatewayError
from ..httpclient import get_json

# 内置最简 SD1.5 工作流（ComfyUI API 格式）。仅当未指定 COMFYUI_WORKFLOW_FILE 时兜底；
# ckpt 名需你机器上存在，正式用建议导出你自己的 work flow 并设 COMFYUI_WORKFLOW_FILE。
DEFAULT_WORKFLOW: dict = {
    "3": {
        "class_type": "KSampler",
        "inputs": {
            "seed": 0,
            "steps": 20,
            "cfg": 7.0,
            "sampler_name": "euler",
            "scheduler": "normal",
            "denoise": 1.0,
            "model": ["4", 0],
            "positive": ["6", 0],
            "negative": ["7", 0],
            "latent_image": ["5", 0],
        },
    },
    "4": {
        "class_type": "CheckpointLoaderSimple",
        "inputs": {"ckpt_name": "v1-5-pruned-emaonly.safetensors"},
    },
    "5": {
        "class_type": "EmptyLatentImage",
        "inputs": {"width": 512, "height": 512, "batch_size": 1},
    },
    "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "", "clip": ["4", 1]}},
    "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "", "clip": ["4", 1]}},
    "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
    "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "smart-assistant"}},
}


def default_workflow_with_ckpt() -> dict:
    """内置默认工作流，但自动挑一个你机器上真实存在的 checkpoint。

    硬编码某个 ckpt 名（如 v1-5-pruned）在你机器上多半不存在，会让 ComfyUI 直接 400。
    故兜底时先查 /object_info 拿到可用 ckpt 列表，取第一个填进节点 4。
    """
    wf = copy.deepcopy(DEFAULT_WORKFLOW)
    try:
        s, b = get_json(
            f"{settings.diffusion_backend_url.rstrip('/')}/object_info/CheckpointLoaderSimple"
        )
        if s == 200:
            opts = json.loads(b)["CheckpointLoaderSimple"]["input"]["required"]["ckpt_name"][0]
            if opts:
                wf["4"]["inputs"]["ckpt_name"] = opts[0]
    except Exception:  # noqa: BLE001
        pass  # 查不到就用默认名，错误会如实上抛给调用方
    return wf


def load_workflow(path: str | None = None) -> dict:
    """载入 ComfyUI 工作流：优先用用户导出的文件，否则用内置默认（自动选 ckpt）。"""
    path = settings.comfyui_workflow_file if path is None else path
    if path:
        try:
            with open(path, "r", encoding="utf-8") as f:
                return json.load(f)
        except Exception as e:  # noqa: BLE001
            raise GatewayError(500, f"读取 ComfyUI 工作流文件失败：{e}")
    return default_workflow_with_ckpt()


def find_node(wf: dict, preferred: str, class_type: str) -> str | None:
    """在图里找节点 id：优先用指定的（且确实存在于图中、且 class_type 匹配），否则按 class_type 取第一个。

    必须校验 class_type：环境变量给的默认 id（如 KSampler=3、Size=5）可能和你导出图里的真实
    节点 id 撞号但类型不同（例如你的图里 3 是正向提示词、5 才是 KSampler）。仅判「id 存在」
    会错配，故优先项也要求类型对得上，否则退回自动识别。
    """
    if preferred and preferred in wf and wf[preferred].get("class_type") == class_type:
        return preferred
    for nid, node in wf.items():
        if node.get("class_type") == class_type:
            return nid
    return None


def resolve_nodes(
    wf: dict,
    *,
    ksampler_node: str | None = None,
    prompt_node: str | None = None,
    negative_node: str | None = None,
    size_node: str | None = None,
) -> dict:
    """解析要注入的节点 id，尽量自动识别，减少手填。

    - KSampler 与其 positive/negative 输入决定了 prompt 落在哪个 CLIPTextEncode；
    - EmptyLatentImage 决定尺寸；都没有则退回环境变量 / None。
    环境变量指定的节点若不在图里（或类型不符）会被忽略，改走自动识别。

    各 `*_node` 参数默认取 `settings`，显式传入可覆盖（便于测试）。
    """
    ksampler_node = settings.comfyui_ksampler_node if ksampler_node is None else ksampler_node
    prompt_node = settings.comfyui_prompt_node if prompt_node is None else prompt_node
    negative_node = settings.comfyui_negative_node if negative_node is None else negative_node
    size_node = settings.comfyui_size_node if size_node is None else size_node

    ks = find_node(wf, ksampler_node, "KSampler")
    pos = neg = None
    if ks:
        inp = wf[ks]["inputs"]
        if isinstance(inp.get("positive"), list) and inp["positive"]:
            pos = inp["positive"][0]
        if isinstance(inp.get("negative"), list) and inp["negative"]:
            neg = inp["negative"][0]
    if pos is None:
        pos = find_node(wf, prompt_node, "CLIPTextEncode")
    if neg is None:
        neg = find_node(wf, negative_node, "CLIPTextEncode")
    size = find_node(wf, size_node, "EmptyLatentImage")
    return {"ksampler": ks, "positive": pos, "negative": neg, "size": size}
