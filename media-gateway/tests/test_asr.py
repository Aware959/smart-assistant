"""ASR 分派 / 错误映射 / 引擎不可用 的契约测试。

对应 `specs/001-telegram-voice-transcription/contracts/transcribe-api.md` 的「测试契约」
一节。全部用**假引擎**注入，不要求真的装 faster-whisper，也不下载模型。

覆盖范围（编号对齐契约）：
1. `ASR_ENGINE=none` -> 503 `engine_unavailable`
2. 引擎依赖不可导入 -> 503，且不抛出未捕获异常
3. 空串 -> 200 + `text == ""`（不是 4xx）
4. 无法解码 -> 400      5. 音频过长 -> 413
6. 正常 -> 200 透传      7. `/health` 依赖不可导入时不报错，`asr.importable == false`
8. 分派层传给引擎的调用含 `vad_filter=True`
9. 预热抛异常时仍能启动，`/health` 仍为 200
另有 415 / 422 / 500 / 空请求体 / 原始字节透传 等补强用例。
"""

from __future__ import annotations

import json
import threading
import unittest
import urllib.error
import urllib.request
from contextlib import contextmanager
from dataclasses import replace
from unittest import mock

import gateway.asr as asr
import gateway.server as server
import tests.fake_asr as fake
from gateway.asr import TranscribeResult

# 假引擎注册项：(适配模块路径, 必须存在的第三方依赖)。
# 依赖故意选标准库 json —— 它一定可导入，于是"引擎可用"这条路径可以走通。
FAKE_ENGINE = ("tests.fake_asr", "json")
# 依赖名指向不存在的模块，用于构造"依赖未安装"。
MISSING_ENGINE = ("tests.fake_asr", "definitely_not_a_real_module_9f3c")

# 绕开系统代理：本机 Clash 等可能把 127.0.0.1 也路由进代理。
_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


@contextmanager
def gateway(**overrides):
    """起一个真实网关（随机端口），并把 ASR 配置换成假引擎。"""
    cfg = replace(server.settings, **{"asr_engine": "fake", **overrides})
    srv = server.GatewayServer(("127.0.0.1", 0), server.Handler)
    srv.daemon_threads = True  # 测试用：别让收尾 join 把用例挂住
    port = srv.server_address[1]
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    with (
        mock.patch.object(server, "settings", cfg),
        mock.patch.object(asr, "settings", cfg),
        mock.patch.dict(asr._ENGINES, {"fake": FAKE_ENGINE, "missing": MISSING_ENGINE}),
    ):
        thread.start()
        try:
            yield f"http://127.0.0.1:{port}"
        finally:
            srv.shutdown()
            srv.server_close()


def _request(url: str, data: bytes | None = None, content_type: str | None = None):
    headers = {"Content-Type": content_type} if content_type else {}
    req = urllib.request.Request(url, data=data, headers=headers)
    try:
        with _OPENER.open(req, timeout=10) as resp:
            return resp.status, json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read().decode("utf-8"))
        finally:
            e.close()


def post_audio(base: str, data: bytes, content_type: str = "audio/ogg"):
    return _request(f"{base}/transcribe", data=data, content_type=content_type)


def get_health(base: str):
    return _request(f"{base}/health")


class TranscribeEndpointTest(unittest.TestCase):
    def setUp(self):
        fake.reset()

    def test_正常转写返回文本与元信息(self):
        with gateway() as base:
            status, body = post_audio(base, b"\x00\x01\x02")
        self.assertEqual(status, 200)
        self.assertEqual(body["text"], "今天天气不错")
        self.assertEqual(body["language"], "zh")
        self.assertAlmostEqual(body["duration"], 1.5)

    def test_原始字节与内容类型原样送达引擎(self):
        payload = bytes(range(64))
        with gateway() as base:
            post_audio(base, payload, "audio/opus")
        self.assertEqual(len(fake.calls), 1)
        self.assertEqual(fake.calls[0]["audio"], payload)
        self.assertEqual(fake.calls[0]["content_type"], "audio/opus")

    def test_分派层强制开启语音段检测(self):
        """不开 VAD 时静音会被 Whisper 幻觉成文本，故这是硬性不变量。"""
        with gateway() as base:
            post_audio(base, b"x")
        self.assertIs(fake.calls[0]["vad_filter"], True)

    def test_空串是正常结果而非错误(self):
        fake.result = TranscribeResult(text="", language="zh", duration=3.0)
        with gateway() as base:
            status, body = post_audio(base, b"x")
        self.assertEqual(status, 200)
        self.assertEqual(body["text"], "")

    def test_元信息缺失时不出现在响应里(self):
        fake.result = TranscribeResult(text="嗯", language=None, duration=None)
        with gateway() as base:
            status, body = post_audio(base, b"x")
        self.assertEqual(status, 200)
        self.assertEqual(body, {"text": "嗯"})

    def test_空请求体归400(self):
        with gateway() as base:
            status, body = post_audio(base, b"")
        self.assertEqual(status, 400)
        self.assertIn("error", body)

    def test_其他路径仍返回404(self):
        with gateway() as base:
            status, body = _request(f"{base}/nope", data=b"x")
        self.assertEqual(status, 404)
        self.assertEqual(body, {"error": "not found"})


class ErrorMappingTest(unittest.TestCase):
    """引擎抛出的错误种类 -> HTTP 状态码（契约的核心）。"""

    def setUp(self):
        fake.reset()

    def _status_for(self, kind: str) -> int:
        fake.fail_with(kind)
        with gateway() as base:
            status, _ = post_audio(base, b"x")
        return status

    def test_无法解码归400(self):
        self.assertEqual(self._status_for("decode"), 400)

    def test_音频过长归413(self):
        self.assertEqual(self._status_for("too_long"), 413)

    def test_格式不支持归415(self):
        self.assertEqual(self._status_for("unsupported"), 415)

    def test_无有效语音归422(self):
        self.assertEqual(self._status_for("no_speech"), 422)

    def test_未知种类归500(self):
        # 引擎用了没登记的种类 -> 不能当成内容问题（否则 4xx 会把我们的 bug 说成用户的音频有问题）。
        self.assertEqual(self._status_for("something_weird"), 500)

    def test_引擎抛出任意异常归500(self):
        fake.error = RuntimeError("引擎炸了")
        with gateway() as base:
            status, body = post_audio(base, b"x")
        self.assertEqual(status, 500)
        self.assertIn("error", body)


class EngineUnavailableTest(unittest.TestCase):
    def setUp(self):
        fake.reset()

    def test_引擎为none时归503并带kind(self):
        with gateway(asr_engine="none") as base:
            status, body = post_audio(base, b"x")
        self.assertEqual(status, 503)
        self.assertEqual(body["kind"], "engine_unavailable")

    def test_引擎名不认识时归503(self):
        with gateway(asr_engine="whisper-cpp") as base:
            status, body = post_audio(base, b"x")
        self.assertEqual(status, 503)
        self.assertEqual(body["kind"], "engine_unavailable")

    def test_依赖未安装时归503(self):
        with gateway(asr_engine="missing") as base:
            status, body = post_audio(base, b"x")
        self.assertEqual(status, 503)
        self.assertEqual(body["kind"], "engine_unavailable")
        self.assertIn("faster-whisper", body["error"])

    def test_依赖未安装不会让进程崩掉(self):
        """一次失败之后，同一进程仍能正常服务别的请求。"""
        with gateway(asr_engine="missing") as base:
            first, _ = post_audio(base, b"x")
            second, _ = get_health(base)
        self.assertEqual(first, 503)
        self.assertEqual(second, 200)


class HealthTest(unittest.TestCase):
    def setUp(self):
        fake.reset()

    def test_健康检查含asr段(self):
        with gateway() as base:
            status, body = get_health(base)
        self.assertEqual(status, 200)
        asr_section = body["asr"]
        self.assertEqual(asr_section["engine"], "fake")
        self.assertIs(asr_section["importable"], True)
        self.assertIs(asr_section["model_loaded"], True)

    def test_模型未加载时如实上报(self):
        fake.is_loaded = False
        with gateway() as base:
            _, body = get_health(base)
        self.assertIs(body["asr"]["model_loaded"], False)

    def test_依赖不可导入时不报错且importable为false(self):
        with gateway(asr_engine="missing") as base:
            status, body = get_health(base)
        self.assertEqual(status, 200)
        self.assertIs(body["asr"]["importable"], False)

    def test_不提供转写时importable为false(self):
        with gateway(asr_engine="none") as base:
            status, body = get_health(base)
        self.assertEqual(status, 200)
        self.assertIs(body["asr"]["importable"], False)

    def test_既有字段保持原样(self):
        """/health 是既有端点，其结构不能被本特性改坏。"""
        with gateway() as base:
            _, body = get_health(base)
        self.assertIs(body["ok"], True)
        self.assertEqual(body["diffusion"], server.settings.diffusion_backend_type)
        self.assertEqual(body["tts"], server.settings.tts_engine)


class EngineImportableTest(unittest.TestCase):
    def test_未配置与未知引擎都算不可用(self):
        self.assertFalse(asr.engine_importable(""))
        self.assertFalse(asr.engine_importable("none"))
        self.assertFalse(asr.engine_importable("whisper-cpp"))

    def test_依赖是否可导入按依赖判断而非适配模块(self):
        """适配模块是我们自己的文件，永远在；判断依据必须是第三方依赖。"""
        with mock.patch.dict(asr._ENGINES, {"fake": FAKE_ENGINE, "missing": MISSING_ENGINE}):
            self.assertTrue(asr.engine_importable("fake"))
            self.assertFalse(asr.engine_importable("missing"))


class WarmupTest(unittest.TestCase):
    def setUp(self):
        fake.reset()

    def test_预热失败只记日志不上抛且健康检查仍为200(self):
        fake.warmup_error = RuntimeError("模型下载失败")
        with gateway() as base:
            ok = asr.warmup(asr.settings)
            status, body = get_health(base)
        self.assertFalse(ok)
        self.assertEqual(status, 200)
        self.assertIs(body["asr"]["importable"], True)

    def test_预热成功返回true(self):
        with gateway():
            self.assertTrue(asr.warmup(asr.settings))

    def test_未配置引擎时预热安静跳过(self):
        with gateway(asr_engine="none"):
            self.assertFalse(asr.warmup(asr.settings))

    def test_依赖未安装时预热安静跳过(self):
        with gateway(asr_engine="missing"):
            self.assertFalse(asr.warmup(asr.settings))


if __name__ == "__main__":
    unittest.main()
