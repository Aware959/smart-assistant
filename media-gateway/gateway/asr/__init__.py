"""语音识别（ASR）：按 `ASR_ENGINE` 分派到具体引擎。

与 `speech/`（TTS）对称，但方向相反：这里**收**原始音频字节、**吐**文本。通道层（Rust）
只跟本层打交道，对背后是哪家引擎无感知——换引擎只改配置。

懒加载是刻意的：本网关的主体能力是出图与出语音，ASR 依赖（faster-whisper 及其 CDN 模型）
是可选的。依赖没装时进程必须**照常启动**，失败只收敛在 `/transcribe` 这一个端点上。

错误归类（契约见 `specs/001-telegram-voice-transcription/contracts/transcribe-api.md`）：

| 情形 | 状态码 | 谁的问题 |
| ---- | ------ | -------- |
| 引擎未配置 / 名不认识 / 依赖装不上 / 模型加载失败 | 503 `engine_unavailable` | 我们 |
| 字节不是可解析的音频 | 400 | 这条音频 |
| 音频超出引擎可处理范围 | 413 | 这条音频 |
| 容器/编码不认识 | 415 | 这条音频 |
| 可解码但没有有效语音 | 422 | 这条音频 |
| 其他内部错误 | 500 | 我们 |

调用方按**状态码**分流（4xx 交给模型自然回应，5xx 走系统故障路径），`kind` 只作补充。
"""

from __future__ import annotations

import importlib
import importlib.util
import sys
from dataclasses import dataclass

from ..config import Settings, settings
from ..errors import GatewayError

# 引擎名 -> (适配模块路径, 必须存在的第三方依赖模块)
#
# 依赖名要单独列出来：适配模块是本仓库自己的文件，永远"可导入"；真正可能缺席的是
# 第三方包（如 faster_whisper 没装）。只看适配模块会把"没装依赖"误判成"可用"。
_ENGINES: dict[str, tuple[str, str]] = {
    "faster-whisper": ("gateway.asr.faster_whisper", "faster_whisper"),
}

# 表示"本网关不提供转写"的引擎名。
ENGINE_NONE = frozenset({"", "none", "off", "null"})

# 转写参数：`vad_filter` 是硬编码的必需项，**故意不提供环境变量开关**。
#
# 不开 VAD 时 Whisper 会把静音/噪音幻觉成真实文本（实测：3 秒静音输出 'You'，
# 且 no_speech_prob 高达 0.862 也照样输出），这段捏造的内容会被当作"用户说的话"
# 进入对话——是正确性缺陷，不是体验问题。开了 VAD 无语音段才会正确地变成空串，
# "空串 ⇒ 没听清"的判据才成立（附带收益：纯噪音音频耗时减半）。
FORCED_VAD_FILTER = True

# 引擎侧错误种类 -> HTTP 状态码。
_KIND_STATUS = {
    "decode": 400,
    "too_long": 413,
    "unsupported": 415,
    "no_speech": 422,
    "internal": 500,
}


class AsrError(Exception):
    """引擎侧的可归类错误：`kind` 决定对外状态码。

    引擎适配层只管抛出它，状态码映射集中在上面的 `_KIND_STATUS`——这样新增引擎不必
    重新背一遍状态码约定。
    """

    def __init__(self, kind: str, detail: str):
        self.kind = kind
        self.detail = detail
        super().__init__(detail)


@dataclass(frozen=True)
class TranscribeResult:
    """一次转写的结果。`text` 为空串表示"音频里没听出内容"，属正常结果而非错误。"""

    text: str
    language: str | None = None
    duration: float | None = None


def _unavailable(detail: str) -> GatewayError:
    return GatewayError(503, detail, kind="engine_unavailable")


def _entry(engine: str) -> tuple[str, str] | None:
    return _ENGINES.get((engine or "").strip().lower())


def engine_importable(engine: str) -> bool:
    """引擎的第三方依赖是否可导入。**不加载模型**——健康检查不该触发 GB 级加载。

    未配置/名不认识一律返回 False（那也确实是"不可用"）。
    """
    entry = _entry(engine)
    if entry is None:
        return False
    _, requires = entry
    try:
        return importlib.util.find_spec(requires) is not None
    except (ImportError, ValueError):  # 父包缺失 / 非法模块名
        return False


def model_loaded(engine: str | None = None) -> bool:
    """模型是否已加载进内存（供 `/health` 与排错用）。

    只看 `sys.modules`（进程内已导入的适配模块），不主动导入、不触发加载。
    """
    entry = _entry(engine if engine is not None else settings.asr_engine)
    if entry is None:
        return False
    module = sys.modules.get(entry[0])
    if module is None:
        return False
    probe = getattr(module, "loaded", None)
    return bool(probe()) if callable(probe) else False


def _load_engine(engine: str):
    """导入引擎适配模块；依赖缺席或模块本身有问题都归为"引擎不可用"。"""
    entry = _entry(engine)
    if entry is None:
        known = "、".join(sorted(_ENGINES)) or "（无）"
        raise _unavailable(f"未知的 ASR 引擎 '{engine}'，可用的有：{known}")
    module_path, requires = entry
    try:
        available = importlib.util.find_spec(requires) is not None
    except (ImportError, ValueError):  # 父包缺失 / 非法模块名
        available = False
    if not available:
        raise _unavailable(
            f"ASR 引擎 '{engine}' 的依赖 {requires} 未安装（pip install faster-whisper \"av<19\"）"
        )
    try:
        return importlib.import_module(module_path)
    except Exception as e:  # noqa: BLE001
        raise _unavailable(f"ASR 引擎 '{engine}' 无法导入：{e}") from e


def transcribe(
    audio: bytes,
    content_type: str,
    config: Settings | None = None,
) -> TranscribeResult:
    """把一段音频字节转成文本。`content_type` 仅作日志与排错。

    失败一律以 `GatewayError` 上抛，状态码语义见模块文档。
    """
    config = config or settings
    engine = (config.asr_engine or "").strip().lower()
    if engine in ENGINE_NONE:
        raise _unavailable("ASR 未启用（ASR_ENGINE 为空或 none）")

    module = _load_engine(engine)
    try:
        return module.transcribe(
            audio,
            content_type,
            config,
            vad_filter=FORCED_VAD_FILTER,
        )
    except AsrError as e:
        raise GatewayError(_KIND_STATUS.get(e.kind, 500), e.detail) from e
    except GatewayError:
        raise
    except Exception as e:  # noqa: BLE001
        raise GatewayError(500, f"ASR 内部错误：{e}") from e


def warmup(config: Settings | None = None) -> bool:
    """启动预热：把模型加载到内存，让第一条语音不必等冷启动。

    **任何失败都只记日志、不上抛**——预热是优化，不是启动前提；失败时 `/transcribe`
    会再试一次并给出明确错误。返回是否成功，供调用方打日志。
    """
    config = config or settings
    engine = (config.asr_engine or "").strip().lower()
    if engine in ENGINE_NONE:
        return False
    try:
        module = _load_engine(engine)
    except GatewayError as e:
        print(f"[asr] 预热跳过：{e.detail}", flush=True)
        return False
    hook = getattr(module, "warmup", None)
    if not callable(hook):
        return False
    try:
        return bool(hook(config))
    except Exception as e:  # noqa: BLE001
        print(f"[asr] 预热失败（不影响启动，转写时会再试一次）：{e}", flush=True)
        return False
