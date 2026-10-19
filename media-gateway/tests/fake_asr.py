"""测试用假 ASR 引擎：零依赖，记录收到的调用参数，按预设返回或抛错。

被 `gateway.asr` 通过 `_ENGINES` 注册表注入（见 tests/test_asr.py），这样契约测试
不需要真的安装 faster-whisper 与下载模型。
"""

from __future__ import annotations

from gateway.asr import AsrError, TranscribeResult

# 每次 transcribe 调用记一条：{"audio": bytes, "content_type": str, "vad_filter": bool}
calls: list[dict] = []

result = TranscribeResult(text="今天天气不错", language="zh", duration=1.5)
error: Exception | None = None
is_loaded = True
warmup_error: Exception | None = None


def reset() -> None:
    """把假引擎恢复到默认状态（每个用例 setUp 调用）。"""
    global result, error, is_loaded, warmup_error
    calls.clear()
    result = TranscribeResult(text="今天天气不错", language="zh", duration=1.5)
    error = None
    is_loaded = True
    warmup_error = None


def loaded() -> bool:
    return is_loaded


def warmup(config) -> bool:  # noqa: ARG001 - 签名对齐真实引擎
    if warmup_error is not None:
        raise warmup_error
    return True


def transcribe(audio, content_type, config, vad_filter=True):  # noqa: ARG001
    calls.append({"audio": audio, "content_type": content_type, "vad_filter": vad_filter})
    if error is not None:
        raise error
    return result


def fail_with(kind: str) -> None:
    """让下一次转写抛出指定种类的引擎错误。"""
    global error
    error = AsrError(kind, f"假引擎故意失败：{kind}")
