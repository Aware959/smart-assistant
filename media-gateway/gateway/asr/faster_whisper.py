"""ASR 引擎适配：`faster-whisper`（懒加载 + 进程内模型单例）。

## 安装前提（实测结论，改之前先读）

- **解释器必须 CPython 3.13**（用 `uv python install 3.13`）。系统默认 3.15 没有
  `ctranslate2` / `onnxruntime` 的 wheel，pip 会退回源码编译 C++，Windows 上通常直接失败。
- **依赖必须锁 `av<19`**。`av 19.x` 删掉了 `av.open(metadata_errors=...)`，而
  faster-whisper 1.2.1 仍在传这个参数 → 装上即崩（`TypeError`）。
- 首次转写会从 HuggingFace 拉模型（`small` 约 484 MB，`tiny` 约 75 MB）。国内网络不通时
  可设 `HF_ENDPOINT=https://hf-mirror.com`，或用 `ASR_MODEL_DIR` 指向已下好的目录。
- 冷启动加载 `small` 实测约 21 秒，故网关在启动时后台预热（见 `ASR_WARMUP`）。

## 为什么这里不用手工转码

PyAV 自带解码能力（内嵌 ffmpeg 库），OGG/OPUS 直接吃——Telegram 语音条正是 OGG/OPUS，
所以送进来的原始字节可以直接喂给 `transcribe()`，不需要先转 16 kHz WAV。

## 注意模块名

本文件叫 `faster_whisper.py`，与第三方包同名。Python 3 的导入是**绝对导入**，`from
faster_whisper import ...` 只解析顶层包（site-packages 里的那个），不会命中本文件
（本文件的绝对名是 `gateway.asr.faster_whisper`）。
"""

from __future__ import annotations

import io
import threading

from ..config import Settings
from . import AsrError, TranscribeResult, _unavailable

# 进程内单例。加载一次要几秒到几十秒，每条语音重载一遍不可接受。
_model = None
_model_lock = threading.Lock()


def loaded() -> bool:
    """模型是否已加载（供 `/health` 探测，不触发加载）。"""
    return _model is not None


def _language(config: Settings) -> str | None:
    """`auto`/空 => None（交给模型自判），否则透传语言码。"""
    raw = (config.asr_language or "").strip()
    return None if raw.lower() in ("", "auto") else raw


def _load(config: Settings):
    """取模型单例；首次调用才真正加载。失败归为"引擎不可用"。"""
    global _model
    if _model is not None:
        return _model
    with _model_lock:
        if _model is not None:  # 双检：等锁期间别的线程可能已加载好
            return _model
        try:
            from faster_whisper import WhisperModel
        except Exception as e:  # noqa: BLE001
            raise _unavailable(f"faster-whisper 无法导入：{e}") from e
        try:
            _model = WhisperModel(
                config.asr_model,
                device=config.asr_device,
                compute_type=config.asr_compute_type,
                download_root=config.asr_model_dir or None,
            )
        except Exception as e:  # noqa: BLE001
            raise _unavailable(
                f"ASR 模型 '{config.asr_model}' 加载失败：{e}（首次使用需联网下载模型）"
            ) from e
        return _model


def warmup(config: Settings) -> bool:
    """启动预热：只做加载。异常交给调用方（`asr.warmup`）兜住。"""
    _load(config)
    return True


def transcribe(
    audio: bytes,
    content_type: str,  # noqa: ARG001 - 仅作日志/排错，PyAV 从字节自行嗅探容器
    config: Settings,
    vad_filter: bool = True,
) -> TranscribeResult:
    """原始音频字节 -> 文本。语音段检测（VAD）由调用方强制打开，默认值只是兜底。"""
    model = _load(config)
    try:
        segments, info = model.transcribe(
            io.BytesIO(audio),  # 字节直接给 PyAV 嗅探容器，不落盘、不转码
            language=_language(config),
            vad_filter=vad_filter,
        )
        # segments 是生成器：不物化就等于没解码。真正的解码/推理都发生在这一步。
        texts = [s.text for s in segments]
    except AsrError:
        raise
    except Exception as e:  # noqa: BLE001
        # 解不出音频（空字节、损坏容器、未知编码）都算"这条音频的问题"。
        raise AsrError("decode", f"无法解码音频：{e}") from e

    return TranscribeResult(
        text="".join(texts).strip(),
        language=getattr(info, "language", None),
        duration=getattr(info, "duration", None),
    )
