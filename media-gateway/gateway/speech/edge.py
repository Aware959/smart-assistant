"""edge-tts 在线音色（可选引擎）。

`edge_tts` 在函数内延迟导入：它依赖 aiohttp，在较新 Python 上未必装得上，若在模块顶层导入
会连图片路径一起拉垮。
"""

from __future__ import annotations

import asyncio
import io

from ..errors import GatewayError


def generate(text: str, voice: str, rate: str) -> tuple[bytes, str]:
    """在线合成 MP3，返回 `(音频字节, MIME)`。"""
    try:
        import edge_tts  # 延迟导入
    except ImportError:
        raise GatewayError(500, "未安装 edge_tts，请 pip install edge-tts")
    communicate = edge_tts.Communicate(text, voice, rate=rate)
    buf = io.BytesIO()

    async def _run():
        async for chunk in communicate.stream():
            if chunk["type"] == "audio":
                buf.write(chunk["data"])

    try:
        asyncio.run(_run())
    except Exception as e:  # noqa: BLE001
        raise GatewayError(502, f"edge-tts 合成失败：{e}")
    return buf.getvalue(), "audio/mpeg"
