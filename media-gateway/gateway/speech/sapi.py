"""语音合成（TTS）：Windows 自带 SAPI 离线合成 + ffmpeg 转码。

SAPI 只能导出 WAV，而 Telegram 语音要 OGG/OPUS 才是原生语音气泡，故合成后用 ffmpeg 转码。
文本经临时文件交给 PowerShell（避免中文在命令行编码里被搞坏），路径/音色/语速经环境变量传，
脚本本体保持纯 ASCII。零 Python 依赖。
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile

from ..config import settings
from ..errors import GatewayError

_SAPI_PS = r"""
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
$want = $env:SA_TTS_VOICE
if ($want -and $want.Trim()) {
  $installed = @($s.GetInstalledVoices() | ForEach-Object { $_.VoiceInfo.Name })
  if ($installed -contains $want) {
    $s.SelectVoice($want)
  } elseif ($want -like 'zh*') {
    # 给的是 edge-tts 风格的 zh-* 音色名而本机没装同名 SAPI 音色时，退到任一中文音色，
    # 免得用英文音色把中文念得面目全非。
    $zh = $s.GetInstalledVoices() |
      Where-Object { $_.VoiceInfo.Culture.Name -like 'zh*' } |
      Select-Object -First 1
    if ($zh) { $s.SelectVoice($zh.VoiceInfo.Name) }
  }
}
$s.Rate = [int]$env:SA_TTS_RATE
$text = Get-Content -Raw -LiteralPath $env:SA_TTS_TXT -Encoding UTF8
$s.SetOutputToWaveFile($env:SA_TTS_WAV)
$s.Speak($text)
$s.Dispose()
"""


def sapi_rate(raw: str) -> int:
    """把 edge-tts 风格的语速（如 `+20%`）映射到 SAPI 的 -10..10 档。

    两者量纲不同（百分比 vs 档位），按「10% ≈ 1 档」粗略换算并夹紧；解析不出就当 0（正常语速）。
    """
    try:
        pct = float(str(raw).strip().rstrip("%"))
    except ValueError:
        pct = 0.0
    return max(-10, min(10, int(round(pct / 10.0))))


def _tail(raw: bytes) -> str:
    """取子进程 stderr 尾部若干字符，用于错误回执。"""
    return raw.decode("utf-8", "ignore")[-500:].strip()


def generate(text: str, voice: str, rate: str) -> tuple[bytes, str]:
    """SAPI 合成 → ffmpeg 转 OGG/OPUS，返回 `(音频字节, MIME)`。"""
    if os.name != "nt":
        raise GatewayError(501, "TTS 引擎 'sapi' 仅支持 Windows")
    ffmpeg = shutil.which(settings.ffmpeg_bin) or (
        settings.ffmpeg_bin
        if os.path.isabs(settings.ffmpeg_bin) and os.path.exists(settings.ffmpeg_bin)
        else None
    )
    if not ffmpeg:
        raise GatewayError(
            500,
            f"找不到 ffmpeg（{settings.ffmpeg_bin}），请安装并加入 PATH，或用 FFMPEG_BIN 指定绝对路径",
        )

    tmp = tempfile.mkdtemp(prefix="sa_tts_")
    txt = os.path.join(tmp, "text.txt")
    wav = os.path.join(tmp, "speech.wav")
    ogg = os.path.join(tmp, "speech.ogg")
    try:
        # 文本落文件（UTF-8），PowerShell 读回，绕开命令行编码对中文的破坏。
        with open(txt, "w", encoding="utf-8") as f:
            f.write(text)
        env = dict(os.environ)
        env.update(
            {
                "SA_TTS_TXT": txt,
                "SA_TTS_WAV": wav,
                "SA_TTS_VOICE": voice or "",
                "SA_TTS_RATE": str(sapi_rate(rate)),
            }
        )
        try:
            proc = subprocess.run(
                [
                    "powershell",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    _SAPI_PS,
                ],
                env=env,
                capture_output=True,
                timeout=settings.http_timeout,
            )
        except FileNotFoundError:
            raise GatewayError(500, "找不到 powershell，无法调用 Windows SAPI")
        except subprocess.TimeoutExpired:
            raise GatewayError(504, "SAPI 合成超时")
        if proc.returncode != 0 or not os.path.exists(wav):
            raise GatewayError(502, f"SAPI 合成失败：{_tail(proc.stderr)}")

        # WAV → OGG/OPUS：Telegram 原生语音格式，单声道 48k，48kbps 足够清晰且体积小。
        try:
            conv = subprocess.run(
                [
                    ffmpeg,
                    "-y",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-i",
                    wav,
                    "-c:a",
                    "libopus",
                    "-b:a",
                    "48k",
                    "-ar",
                    "48000",
                    "-ac",
                    "1",
                    "-f",
                    "ogg",
                    ogg,
                ],
                capture_output=True,
                timeout=settings.http_timeout,
            )
        except subprocess.TimeoutExpired:
            raise GatewayError(504, "ffmpeg 转码超时")
        if conv.returncode != 0 or not os.path.exists(ogg):
            raise GatewayError(502, f"ffmpeg 转码失败：{_tail(conv.stderr)}")

        with open(ogg, "rb") as f:
            return f.read(), "audio/ogg"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
