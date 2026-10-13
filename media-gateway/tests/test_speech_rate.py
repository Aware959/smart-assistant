"""语速换算测试（纯函数）。"""

import unittest

from gateway.speech.sapi import sapi_rate


class SapiRateTest(unittest.TestCase):
    def test_positive_and_negative(self):
        self.assertEqual(sapi_rate("+20%"), 2)
        self.assertEqual(sapi_rate("-20%"), -2)

    def test_zero_and_unparsable(self):
        self.assertEqual(sapi_rate("0%"), 0)
        self.assertEqual(sapi_rate("garbage"), 0)
        self.assertEqual(sapi_rate(""), 0)

    def test_clamped_to_sapi_range(self):
        self.assertEqual(sapi_rate("+1000%"), 10)
        self.assertEqual(sapi_rate("-1000%"), -10)


if __name__ == "__main__":
    unittest.main()
