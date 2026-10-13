"""HTTP 服务：路由、启动与退出。

| 方法 | 路径               | 返回                                  |
| ---- | ------------------ | ------------------------------------- |
| GET  | `/health`          | 配置摘要（扩散后端 / TTS 引擎）        |
| POST | `/generate/image`  | 图片字节                              |
| POST | `/generate/voice`  | 音频字节                              |

退出语义（对齐 Rust 侧 `sa-link`）：

- 第一次 Ctrl+C / Ctrl+Break：停止接收新请求，并等**在途**请求（出图 / 合成）跑完再退出；
- 第二次：不再等待，直接强制退出。
"""

from __future__ import annotations

import json
import os
import signal
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from .config import Settings, settings
from .errors import GatewayError
from .imaging import generate_image
from .speech import generate_voice


class Handler(BaseHTTPRequestHandler):
    def _send(self, status: int, body: bytes, ctype: str = "application/json"):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def _json(self) -> dict:
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        return json.loads(raw.decode("utf-8") or "{}")

    def do_GET(self):
        if self.path.split("?")[0] == "/health":
            body = {"ok": True, "diffusion": settings.diffusion_backend_type, "tts": settings.tts_engine}
            self._send(200, json.dumps(body).encode())
            return
        self._send(404, json.dumps({"error": "not found"}).encode())

    def do_POST(self):
        try:
            if self.path.split("?")[0] == "/generate/image":
                data, ctype = generate_image(self._json())
                self._send(200, data, ctype)
            elif self.path.split("?")[0] == "/generate/voice":
                data, ctype = generate_voice(self._json())
                self._send(200, data, ctype)
            else:
                self._send(404, json.dumps({"error": "not found"}).encode())
        except GatewayError as e:
            self._send(e.status, json.dumps({"error": e.detail}).encode())
        except Exception as e:  # noqa: BLE001
            self._send(500, json.dumps({"error": f"内部错误：{e}"}).encode())

    def log_message(self, fmt, *args):  # 静默默认访问日志
        return


class GatewayServer(ThreadingHTTPServer):
    # 关掉 SO_REUSEADDR（http.server 默认 allow_reuse_address=1）。该选项在 Windows 上
    # 允许另一个套接字"抢绑"同一端口，会让两个网关进程静默并存；关掉后端口被占时
    # 直接报错退出。实测本地「关闭后立即重启」不受影响（TIME_WAIT 不挡监听套接字重绑）。
    allow_reuse_address = False


def serve(config: Settings | None = None):
    """阻塞式启动 HTTP 服务（只监听 127.0.0.1），并接管退出信号。"""
    config = config or settings
    try:
        server = GatewayServer(("127.0.0.1", config.port), Handler)
    except OSError as e:
        print(
            f"启动失败：无法绑定 127.0.0.1:{config.port}（{e.strerror or e}）。"
            f"端口可能已被另一个 media-gateway 占用。",
            flush=True,
        )
        raise SystemExit(1) from e
    # 让 server_close() 等在途请求线程收尾（ThreadingHTTPServer 默认是 daemon，会被直接砍掉）。
    server.daemon_threads = False

    stopping = threading.Event()  # 已收到第一次退出信号
    finished = threading.Event()  # 收尾（关套接字 + join 在途请求）已完成
    drain_started = threading.Event()

    def _drain():
        # 放到独立线程：server_close() 会 join 在途请求线程，若在主线程做这一步，
        # 主线程会卡在 C 层 join 里，导致第二次信号的处理函数迟迟得不到执行。
        server.shutdown()  # 停止接受新连接，serve_forever 返回
        server.server_close()  # 关监听套接字 + join 在途请求线程
        finished.set()

    def _ensure_drain():
        if not drain_started.is_set():
            drain_started.set()
            threading.Thread(target=_drain, daemon=True).start()

    def _on_signal(signum, _frame):
        if stopping.is_set():
            print("\n再次收到退出信号，强制退出。", flush=True)
            os._exit(1)
        stopping.set()
        print(
            f"\n收到退出信号({signum})：停止接收新请求，等待在途请求收尾……再按一次可强制退出。",
            flush=True,
        )
        _ensure_drain()

    for sig in (signal.SIGINT, getattr(signal, "SIGBREAK", None), signal.SIGTERM):
        if sig is None:
            continue
        try:
            signal.signal(sig, _on_signal)
        except (ValueError, OSError):
            pass  # 非主线程或平台不支持，退回 KeyboardInterrupt 兜底

    print(
        f"media-gateway 监听 http://127.0.0.1:{config.port}  "
        f"(diffusion={config.diffusion_backend_type}, Ctrl+C / Ctrl+Break 退出)",
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:  # 兜底：信号处理器没挂上时（如非主线程调用）
        pass
    finally:
        _ensure_drain()
        # 轮询等待收尾完成（而非直接 join）：保持主线程可继续处理信号，
        # 这样收尾期间再按一次退出能立即强制退出。
        while not finished.is_set():
            time.sleep(0.05)
        print("media-gateway 已退出", flush=True)
