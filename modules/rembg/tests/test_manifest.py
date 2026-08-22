"""module.toml 关键字段断言（tomllib，系统 python 可跑）。

W1/WS-C 交付验收点：backends 诚实化 + requirements_by_backend(M2) +
compute.env 设备注入契约。
"""

import tomllib
import unittest
from pathlib import Path

MODULE_DIR = Path(__file__).resolve().parents[1]
MANIFEST_PATH = MODULE_DIR / "module.toml"


class RembgManifestTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.m = tomllib.loads(MANIFEST_PATH.read_text(encoding="utf-8"))

    def test_identity(self):
        self.assertEqual(self.m["module"]["id"], "rembg")
        self.assertTrue(MANIFEST_PATH.is_file())

    def test_backends_honest_openvino_first_cpu_last(self):
        self.assertEqual(self.m["compute"]["backends"], ["openvino", "cpu"])

    def test_requirements_by_backend_openvino_file(self):
        rbb = self.m["runtime"]["requirements_by_backend"]
        self.assertEqual(rbb.get("openvino"), "requirements-openvino.txt")
        self.assertTrue((MODULE_DIR / rbb["openvino"]).is_file())

    def test_base_requirements_still_declared(self):
        req = self.m["runtime"]["requirements"]
        self.assertEqual(req, "requirements.txt")
        self.assertTrue((MODULE_DIR / req).is_file())

    def test_compute_env_injects_openvino_device(self):
        env = self.m["compute"]["env"]["openvino"]
        self.assertEqual(env["OPENVINO_DEVICE"], "{device_name}")

    def test_models_variants_intact(self):
        ids = {m["id"] for m in self.m["models"]}
        self.assertLessEqual({"u2net", "isnet-general-use", "birefnet-general"}, ids)

    def test_version_bumped_for_backend_support(self):
        parts = self.m["module"]["version"].split(".")
        self.assertEqual(len(parts), 3)
        self.assertGreaterEqual(tuple(int(p) for p in parts), (2, 1, 0))


if __name__ == "__main__":
    unittest.main()
