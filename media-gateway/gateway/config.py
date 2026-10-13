"""配置：加载 .env，并把环境变量整理成不可变的 Settings。

优先级：进程环境变量 > .env > 代码默认值（.env 里已有的键不覆盖进程环境，与 Rust 后端
的 dotenvy 同语义）。

环境变量：
    PORT                  监听端口（默认 8787，需与 backend 的 MEDIA_GATEWAY_URL 一致）
    DIFFUSION_BACKEND_URL 扩散后端 API 基址（默认 http://127.0.0.1:8188）
    DIFFUSION_BACKEND_TYPE a1111（默认）| comfyui
    COMFYUI_WORKFLOW_FILE 你导出的 API 格式工作流 JSON；相对路径按 media-gateway/ 解析
    COMFYUI_PROMPT_NODE / _NEGATIVE_NODE / _SIZE_NODE / _KSAMPLER_NODE  可选节点 id 覆盖
    TTS_ENGINE            sapi（默认，Windows 离线）| edge-tts（联网，需装 edge-tts）
    TTS_VOICE             音色名（sapi 用 SAPI 音色全名；edge-tts 用其音色名）
    SAPI_VOICE            可选：显式指定 SAPI 音色全名（优先级高于 TTS_VOICE）
    FFMPEG_BIN            ffmpeg 可执行文件（默认取 PATH 里的 ffmpeg；sapi 引擎用它转码）
    MEDIA_HTTP_TIMEOUT    单次生成总超时秒（默认 120）
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

# media-gateway/ 目录（本包位于其下的 gateway/ 子目录）
BASE_DIR = Path(__file__).resolve().parent.parent


def load_dotenv(path: str | os.PathLike[str] = ".env") -> None:
    """极简 .env 加载（标准库，零依赖）。进程已存在的环境变量优先，不覆盖。

    相对路径按 media-gateway/ 目录解析。.env 里写的键会被设进进程环境，从而让 urllib
    自动读到 NO_PROXY 等。
    """
    full = Path(path)
    if not full.is_absolute():
        full = BASE_DIR / full
    if not full.exists():
        return
    with open(full, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, val = line.split("=", 1)
            key, val = key.strip(), val.strip()
            # 去掉成对的单/双引号。
            if len(val) >= 2 and val[0] == val[-1] and val[0] in ("'", '"'):
                val = val[1:-1]
            if key and key not in os.environ:
                os.environ[key] = val


@dataclass(frozen=True)
class Settings:
    """一次进程内不变的运行配置。"""

    port: int
    diffusion_backend_url: str
    diffusion_backend_type: str
    comfyui_workflow_file: str  # 已解析为绝对路径；空串表示用内置兜底工作流
    comfyui_prompt_node: str
    comfyui_negative_node: str
    comfyui_size_node: str
    comfyui_ksampler_node: str
    tts_engine: str
    tts_voice: str
    sapi_voice: str
    ffmpeg_bin: str
    http_timeout: float

    @classmethod
    def from_env(cls) -> "Settings":
        workflow_file = os.getenv("COMFYUI_WORKFLOW_FILE", "").strip()
        # 相对路径按 media-gateway/ 解析，避免"只有在某目录下启动才找得到"的坑。
        if workflow_file and not Path(workflow_file).is_absolute():
            workflow_file = str(BASE_DIR / workflow_file)
        return cls(
            port=int(os.getenv("PORT", "8787")),
            diffusion_backend_url=os.getenv("DIFFUSION_BACKEND_URL", "http://127.0.0.1:8188"),
            diffusion_backend_type=os.getenv("DIFFUSION_BACKEND_TYPE", "a1111").lower(),
            comfyui_workflow_file=workflow_file,
            comfyui_prompt_node=os.getenv("COMFYUI_PROMPT_NODE", "6"),
            comfyui_negative_node=os.getenv("COMFYUI_NEGATIVE_NODE", "7"),
            comfyui_size_node=os.getenv("COMFYUI_SIZE_NODE", "5"),
            comfyui_ksampler_node=os.getenv("COMFYUI_KSAMPLER_NODE", "3"),
            tts_engine=os.getenv("TTS_ENGINE", "sapi").lower(),
            tts_voice=os.getenv("TTS_VOICE", "zh-CN-XiaoxiaoNeural"),
            sapi_voice=os.getenv("SAPI_VOICE", ""),
            ffmpeg_bin=os.getenv("FFMPEG_BIN", "ffmpeg"),
            http_timeout=float(os.getenv("MEDIA_HTTP_TIMEOUT", "120")),
        )


load_dotenv()  # 必须在 from_env 之前
settings = Settings.from_env()
