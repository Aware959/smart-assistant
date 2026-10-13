"""出图：按 `DIFFUSION_BACKEND_TYPE` 分派到具体后端。"""

from __future__ import annotations

from ..config import settings
from ..errors import GatewayError
from . import a1111, comfyui


def generate_image(req: dict) -> tuple[bytes, str]:
    """统一入口：返回 `(图片字节, MIME)`，供通道层直接上传。"""
    backend = settings.diffusion_backend_type
    if backend == "a1111":
        return a1111.generate(req)
    if backend == "comfyui":
        return comfyui.generate(req)
    raise GatewayError(501, f"diffusion backend '{backend}' 暂不支持，目前仅 a1111 / comfyui")
