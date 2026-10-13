"""本机媒体网关入口：让 smart-assistant 调用你电脑上的扩散软件与 TTS 出图/出声。

这是独立进程，与 Rust 后端解耦——后端只通过 HTTP 调这里的 `/generate/image`、`/generate/voice`，
至于背后是 StabilityMatrix 前端的 A1111、ComfyUI 还是别的，后端一概不知。

**纯标准库实现**（http.server + urllib），不依赖 fastapi/uvicorn/httpx，避免在较新 Python
（如 3.15）上与 anyio 等异步依赖打架。语音默认走 Windows SAPI（系统自带，零 Python 依赖）
+ ffmpeg 转 OGG/OPUS，离线可用；也可切回 edge-tts 在线音色。

启动：
    python main.py              # 监听 127.0.0.1:8787（图片零依赖；语音需 ffmpeg 在 PATH）

配置见同目录 `.env`（完整变量列在 `gateway/config.py` 顶部 docstring 与 `.env.example`）。
代码结构见 `gateway/` 包与 README 的「代码结构」一节。
"""

from __future__ import annotations

from gateway.server import serve


def main() -> None:
    serve()


if __name__ == "__main__":
    main()
