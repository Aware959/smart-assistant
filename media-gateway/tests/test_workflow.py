"""ComfyUI 工作流节点识别与提示词注入测试（纯函数，无网络）。"""

import unittest

from gateway.imaging.workflow import apply_prompt, find_node, inject_prompt, resolve_nodes

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


class InjectPromptTest(unittest.TestCase):
    def test_placeholder_is_replaced_and_template_tags_kept(self):
        out = inject_prompt("{prompt}, masterpiece, best quality", "a cat on a windowsill")
        self.assertEqual(out, "a cat on a windowsill, masterpiece, best quality")

    def test_placeholder_can_sit_inside_the_template(self):
        out = inject_prompt("masterpiece, {prompt}, 8k", "a cat")
        self.assertEqual(out, "masterpiece, a cat, 8k")

    def test_empty_template_yields_prompt_only(self):
        # 内置兜底工作流的正向节点就是空的 → 提示词即全文。
        self.assertEqual(inject_prompt("", "a cat"), "a cat")

    def test_template_without_placeholder_keeps_its_words(self):
        out = inject_prompt("masterpiece, best quality", "a cat")
        self.assertEqual(out, "masterpiece, best quality, a cat")

    def test_braces_in_template_do_not_break_injection(self):
        # 用 replace 而非 format：模板里的花括号（LoRA 权重等）不该被当字段名解析。
        out = inject_prompt("{lora:1.2} {prompt}", "a cat")
        self.assertEqual(out, "{lora:1.2} a cat")


class ApplyPromptTest(unittest.TestCase):
    def _workflow(self, positive_text, negative_text="old negative"):
        wf = {
            "3": {"class_type": "CLIPTextEncode", "inputs": {"text": positive_text}},
            "4": {"class_type": "CLIPTextEncode", "inputs": {"text": negative_text}},
            "5": {
                "class_type": "KSampler",
                "inputs": {"positive": ["3", 0], "negative": ["4", 0]},
            },
            "6": {"class_type": "EmptyLatentImage", "inputs": {"width": 832, "height": 1024}},
        }
        return wf, resolve_nodes(wf)

    def test_positive_prompt_goes_through_the_template(self):
        wf, nodes = self._workflow("{prompt}, masterpiece, best quality")
        apply_prompt(wf, nodes, {"prompt": "a cat"})
        self.assertEqual(wf["3"]["inputs"]["text"], "a cat, masterpiece, best quality")

    def test_negative_prompt_is_left_untouched(self):
        # 负面词是模板作者的调校结果：请求没带 negative_prompt 时不许被清空。
        wf, nodes = self._workflow("{prompt}", negative_text="lowres, bad hands, watermark")
        apply_prompt(wf, nodes, {"prompt": "a cat"})
        self.assertEqual(wf["4"]["inputs"]["text"], "lowres, bad hands, watermark")

    def test_size_kept_when_request_omits_it(self):
        wf, nodes = self._workflow("{prompt}")
        apply_prompt(wf, nodes, {"prompt": "a cat"})
        self.assertEqual(wf["6"]["inputs"]["width"], 832)
        self.assertEqual(wf["6"]["inputs"]["height"], 1024)

    def test_size_overridden_when_request_provides_it(self):
        wf, nodes = self._workflow("{prompt}")
        apply_prompt(wf, nodes, {"prompt": "a cat", "width": 512, "height": 512})
        self.assertEqual(wf["6"]["inputs"]["width"], 512)
        self.assertEqual(wf["6"]["inputs"]["height"], 512)


if __name__ == "__main__":
    unittest.main()
