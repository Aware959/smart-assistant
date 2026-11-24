"""MCP 协议层与工具表测试（不触网）。

重点守住三件事：
- 通知（无 id）不回应，非 JSON 行不崩；
- 工具执行失败是**业务结果**（`isError: true`），不是 JSON-RPC 错误；
- 工具表里确实有 `generate_image`，且它的入参要求 `prompt`。
"""

import json
import unittest
from io import StringIO

from comfy import protocol, tools


def _serve(lines, handler=None):
    """把若干行喂给协议层，返回解析后的回应列表。"""
    out = StringIO()
    handler = handler or (lambda method, params: {"method": method})
    protocol.serve(handler, stdin=iter(lines), stdout=out)
    return [json.loads(line) for line in out.getvalue().splitlines() if line.strip()]


class ProtocolTest(unittest.TestCase):
    def test_request_gets_one_response(self):
        responses = _serve([json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})])
        self.assertEqual(len(responses), 1)
        self.assertEqual(responses[0]["id"], 1)
        self.assertEqual(responses[0]["result"]["method"], "tools/list")

    def test_notification_is_ignored(self):
        responses = _serve(
            [json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})]
        )
        self.assertEqual(responses, [])

    def test_non_json_line_does_not_break_the_loop(self):
        responses = _serve(
            [
                "这不是 JSON",
                json.dumps({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}),
            ]
        )
        self.assertEqual(len(responses), 1)
        self.assertEqual(responses[0]["id"], 7)

    def test_handler_error_becomes_json_rpc_error(self):
        def boom(method, params):
            raise RuntimeError("炸了")

        responses = _serve(
            [json.dumps({"jsonrpc": "2.0", "id": 3, "method": "tools/list"})],
            handler=boom,
        )
        self.assertEqual(len(responses), 1)
        self.assertIn("error", responses[0])
        self.assertIn("炸了", responses[0]["error"]["message"])


class ToolsTest(unittest.TestCase):
    def test_tool_list_contains_generate_image(self):
        listed = tools.list_tools()["tools"]
        names = [t["name"] for t in listed]
        self.assertIn("generate_image", names)
        self.assertIn("server_info", names)
        schema = next(t for t in listed if t["name"] == "generate_image")["inputSchema"]
        self.assertEqual(schema["required"], ["prompt"])

    def test_unknown_tool_is_business_error_not_protocol_error(self):
        outcome = tools.call_tool("no_such_tool", {})
        self.assertTrue(outcome["isError"])
        self.assertIn("no_such_tool", outcome["content"][0]["text"])

    def test_missing_prompt_is_business_error(self):
        outcome = tools.call_tool("generate_image", {})
        self.assertTrue(outcome["isError"])
        self.assertIn("prompt", outcome["content"][0]["text"])

    def test_dispatch_handles_initialize_and_list(self):
        init = tools.dispatch("initialize", {})
        self.assertIn("protocolVersion", init)
        self.assertIn("tools", init["capabilities"])
        listed = tools.dispatch("tools/list", {})
        self.assertTrue(listed["tools"])

    def test_dispatch_rejects_unknown_method(self):
        with self.assertRaises(KeyError):
            tools.dispatch("no/such/method", {})


if __name__ == "__main__":
    unittest.main()
