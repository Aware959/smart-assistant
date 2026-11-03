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

import argparse
import os


def main() -> None:
    parser = argparse.ArgumentParser(description="知微本机媒体网关")
    parser.add_argument(
        "--env",
        metavar="PATH",
        help="配置文件路径（默认同目录 media-gateway.env，其次 .env）",
    )
    args = parser.parse_args()
    if args.env:
        os.environ["MEDIA_GATEWAY_ENV"] = args.env

    # 必须在设置 MEDIA_GATEWAY_ENV 之后再导入：config 在模块导入时就会读取配置。
    from gateway.server import serve

    serve()


if __name__ == "__main__":
    main()
