"""工作流文件加载测试（用临时文件，不触网）。"""

import json
import os
import tempfile
import unittest

from gateway.errors import GatewayError
from gateway.imaging.workflow import load_workflow


class LoadWorkflowTest(unittest.TestCase):
    def test_loads_explicit_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "wf.json")
            with open(path, "w", encoding="utf-8") as f:
                json.dump({"1": {"class_type": "KSampler", "inputs": {}}}, f)
            wf = load_workflow(path)
        self.assertIn("1", wf)

    def test_missing_file_raises_500(self):
        with self.assertRaises(GatewayError) as cm:
            load_workflow(os.path.join(tempfile.gettempdir(), "definitely_not_here_9527.json"))
        self.assertEqual(cm.exception.status, 500)


if __name__ == "__main__":
    unittest.main()
