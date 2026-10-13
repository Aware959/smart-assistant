"""ComfyUI 工作流节点识别测试（纯函数，无网络）。"""

import unittest

from gateway.imaging.workflow import find_node, resolve_nodes

# 模拟一份用户导出的工作流：节点 id 与 .env 里的默认覆盖值故意错位，
# 以验证「自动识别优先于错配的 id 覆盖」。
SAMPLE = {
    "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "x.safetensors"}},
    "2": {"class_type": "CLIPSetLastLayer", "inputs": {"clip": ["1", 1]}},
    "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "old positive"}},
    "4": {"class_type": "CLIPTextEncode", "inputs": {"text": "old negative"}},
    "5": {
        "class_type": "KSampler",
        "inputs": {"positive": ["3", 0], "negative": ["4", 0], "latent_image": ["6", 0]},
    },
    "6": {"class_type": "EmptyLatentImage", "inputs": {"width": 832, "height": 1024}},
    "7": {"class_type": "VAEDecode", "inputs": {"samples": ["5", 0]}},
    "8": {"class_type": "SaveImage", "inputs": {"images": ["7", 0]}},
}


class FindNodeTest(unittest.TestCase):
    def test_preferred_wins_when_id_and_type_match(self):
        self.assertEqual(find_node(SAMPLE, "6", "EmptyLatentImage"), "6")

    def test_preferred_ignored_when_id_exists_but_type_differs(self):
        # 默认 KSampler=3，但这里的 3 是 CLIPTextEncode → 应退回按类型找，得到 5。
        self.assertEqual(find_node(SAMPLE, "3", "KSampler"), "5")

    def test_missing_class_returns_none(self):
        self.assertIsNone(find_node(SAMPLE, "", "LoraLoader"))


class ResolveNodesTest(unittest.TestCase):
    def test_follows_ksampler_io_and_autodetects_size(self):
        # 传入与真实节点错位的默认 id，仍应解析出正确的 5/3/4/6。
        nodes = resolve_nodes(SAMPLE, ksampler_node="3", prompt_node="6", negative_node="7", size_node="5")
        self.assertEqual(nodes["ksampler"], "5")
        self.assertEqual(nodes["positive"], "3")
        self.assertEqual(nodes["negative"], "4")
        self.assertEqual(nodes["size"], "6")

    def test_explicit_override_respected_when_matching(self):
        nodes = resolve_nodes(SAMPLE, ksampler_node="5", size_node="6")
        self.assertEqual(nodes["ksampler"], "5")
        self.assertEqual(nodes["size"], "6")


if __name__ == "__main__":
    unittest.main()
