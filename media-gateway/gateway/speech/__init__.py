"""语音：按 `TTS_ENGINE` 分派到具体引擎。"""

from __future__ import annotations

from ..config import settings
from ..errors import GatewayError
from . import edge, sapi


def generate_voice(req: dict) -> tuple[bytes, str]:
    """统一入口：返回 `(音频字节, MIME)`。

    两个引擎都产出 Telegram `sendVoice` 能直接吃的格式（sapi → OGG/OPUS，edge-tts → MP3），
    所以通道层对引擎无感知，只按字节上传。
    """
    text = (req.get("text") or "").strip()
    if not text:
        raise GatewayError(400, "text 不能为空")
    voice = req.get("voice") or settings.sapi_voice or settings.tts_voice
    rate = req.get("rate") or "+0%"
    engine = settings.tts_engine
    if engine == "sapi":
        return sapi.generate(text, voice, rate)
    if engine == "edge-tts":
        return edge.generate(text, voice, rate)
    raise GatewayError(501, f"TTS 引擎 '{engine}' 尚未实现，目前仅 sapi / edge-tts")
