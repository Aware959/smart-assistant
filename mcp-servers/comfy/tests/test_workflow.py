"""工作流节点识别与提示词注入测试（纯函数，无网络）。

用例沿用旧媒体网关的那一套：模拟一份用户导出的工作流，节点 id 与配置里的默认
覆盖值故意错位，以验证「自动识别优先于错配的 id 覆盖」。
"""

import unittest

from comfy.workflow import apply_prompt, find_node, inject_prompt, resolve_nodes

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
        nodes = resolve_nodes(
            SAMPLE,
            ksampler_node="3",
            prompt_node="6",
            negative_node="7",
            size_node="5",
        )
        self.assertEqual(nodes["ksampler"], "5")
        self.assertEqual(nodes["positive"], "3")
        self.assertEqual(nodes["negative"], "4")
        self.assertEqual(nodes["size"], "6")

    def test_explicit_override_respected_when_matching(self):
        nodes = resolve_nodes(SAMPLE, ksampler_node="5", size_node="6")
        self.assertEqual(nodes["ksampler"], "5")
        self.assertEqual(nodes["size"], "6")


class InjectPromptTest(unittest.TestCase):
    def test_placeholder_is_replaced_and_template_tags_kept(self):
        out = inject_prompt("{prompt}, masterpiece, best quality", "a cat on a windowsill")
        self.assertEqual(out, "a cat on a windowsill, masterpiece, best quality")

    def test_placeholder_can_sit_inside_the_template(self):
        out = inject_prompt("masterpiece, {prompt}, 8k", "a cat")
        self.assertEqual(out, "masterpiece, a cat, 8k")

    def test_empty_template_yields_prompt_only(self):
        self.assertEqual(inject_prompt("", "a cat"), "a cat")

    def test_template_without_placeholder_keeps_its_words(self):
        out = inject_prompt("masterpiece, best quality", "a cat")
        self.assertIn("masterpiece, best quality", out)
        self.assertIn("a cat", out)


class ApplyPromptTest(unittest.TestCase):
    def test_positive_replaced_negative_untouched(self):
        wf = {k: dict(v) for k, v in SAMPLE.items()}
        nodes = resolve_nodes(wf)
        apply_prompt(wf, nodes, {"prompt": "a girl with silver hair"})
        # 模板里的词是风格的一部分（没有占位符时是**追加**，不是替换）。
        self.assertIn("a girl with silver hair", wf["3"]["inputs"]["text"])
        self.assertIn("old positive", wf["3"]["inputs"]["text"])
        # 负向提示词是模板作者的调校结果，不该被请求覆盖。
        self.assertEqual(wf["4"]["inputs"]["text"], "old negative")

    def test_size_overwritten_only_when_both_given(self):
        wf = {k: dict(v) for k, v in SAMPLE.items()}
        nodes = resolve_nodes(wf)
        apply_prompt(wf, nodes, {"prompt": "x", "width": 512, "height": 512})
        self.assertEqual(wf["6"]["inputs"]["width"], 512)
        self.assertEqual(wf["6"]["inputs"]["height"], 512)


if __name__ == "__main__":
    unittest.main()
