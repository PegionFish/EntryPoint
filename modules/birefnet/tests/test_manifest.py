"""module.toml 关键字段断言（tomllib，系统 python 可跑）。

W1/WS-C 交付验收点：category/genre、backends 词表与默认后端、
requirements_by_backend(M2) 四文件齐备、capability matte(image→image)、
双变体模型声明（ZhengPeng7 上游出处 + 变体级显存估算）。
"""

import tomllib
import unittest
from pathlib import Path

MODULE_DIR = Path(__file__).resolve().parents[1]
MANIFEST_PATH = MODULE_DIR / "module.toml"


class OnnxMattingManifestTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.m = tomllib.loads(MANIFEST_PATH.read_text(encoding="utf-8"))

    def test_identity_and_classification(self):
        mod = self.m["module"]
        self.assertEqual(mod["id"], "onnx-matting")
        self.assertEqual(mod["category"], "image")
        self.assertEqual(mod["genre"], "matting")

    def test_backends_order_and_default(self):
        compute = self.m["compute"]
        self.assertEqual(compute["backends"], ["cuda", "rocm", "openvino", "cpu"])
        self.assertEqual(compute["default_backend"], "cuda")

    def test_requirements_by_backend_all_three_files(self):
        rbb = self.m["runtime"]["requirements_by_backend"]
        self.assertEqual(
            rbb,
            {
                "cuda": "requirements-cuda.txt",
                "rocm": "requirements-rocm.txt",
                "openvino": "requirements-openvino.txt",
            },
        )
        for rel in rbb.values():
            self.assertTrue((MODULE_DIR / rel).is_file(), f"missing file: {rel}")
        # cpu 回退 requirements.txt 必须存在
        self.assertTrue((MODULE_DIR / self.m["runtime"]["requirements"]).is_file())

    def test_compute_env_device_injection(self):
        env = self.m["compute"]["env"]
        self.assertEqual(env["cuda"]["CUDA_VISIBLE_DEVICES"], "{device_index}")
        self.assertEqual(env["rocm"]["HIP_VISIBLE_DEVICES"], "{device_index}")
        self.assertEqual(env["openvino"]["OPENVINO_DEVICE"], "{device_name}")

    def test_capability_matte_image_to_image(self):
        caps = {c["name"]: c for c in self.m["interface"]["capabilities"]}
        self.assertIn("matte", caps)
        cap = caps["matte"]
        self.assertEqual(cap["input_type"], "image")
        self.assertEqual(cap["output_type"], "image")

    def test_models_two_variants_upstream_provenance(self):
        models = {m["id"]: m for m in self.m["models"]}
        self.assertLessEqual({"birefnet-general", "birefnet-portrait"}, set(models))
        self.assertEqual(models["birefnet-general"]["source"], "huggingface")
        self.assertEqual(models["birefnet-general"]["repo_id"], "ZhengPeng7/BiRefNet")
        self.assertEqual(models["birefnet-portrait"]["repo_id"], "ZhengPeng7/BiRefNet-portrait")
        self.assertTrue(models["birefnet-general"].get("default"))

    def test_variant_vram_estimates_track_weights(self):
        models = {m["id"]: m for m in self.m["models"]}
        self.assertGreaterEqual(models["birefnet-general"]["vram_estimate_mb"], 1000)
        self.assertGreaterEqual(models["birefnet-portrait"]["vram_estimate_mb"], 972)


if __name__ == "__main__":
    unittest.main()
