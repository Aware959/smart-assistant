"""分派与错误路径测试（覆盖 Settings 覆盖与错误码，不触网）。"""

import unittest
from dataclasses import replace
from unittest import mock

import gateway.imaging as imaging
import gateway.speech as speech
from gateway.errors import GatewayError


class ImageDispatchTest(unittest.TestCase):
    def test_unknown_backend_raises_501(self):
        with mock.patch.object(imaging, "settings", replace(imaging.settings, diffusion_backend_type="nope")):
            with self.assertRaises(GatewayError) as cm:
                imaging.generate_image({"prompt": "x"})
        self.assertEqual(cm.exception.status, 501)


class VoiceDispatchTest(unittest.TestCase):
    def test_empty_text_raises_400(self):
        with self.assertRaises(GatewayError) as cm:
            speech.generate_voice({"text": "   "})
        self.assertEqual(cm.exception.status, 400)

    def test_unknown_engine_raises_501(self):
        with mock.patch.object(speech, "settings", replace(speech.settings, tts_engine="nope")):
            with self.assertRaises(GatewayError) as cm:
                speech.generate_voice({"text": "hi"})
        self.assertEqual(cm.exception.status, 501)


if __name__ == "__main__":
    unittest.main()
