"""qwen3-asr 模块清单/适配器静态验证（stdlib only，可直接运行）：

    python3 modules/qwen3-asr/tests/test_module.py

覆盖任务书验收口径：
  1) tomllib 断言 module.toml 关键字段（category/genre/models/capabilities/vram）
  2) python3 -m py_compile 等价的 adapter.py 编译检查
  3) requirements.txt 必备依赖行（fastapi/uvicorn/python-multipart/qwen-asr/torch pin）
  4) adapter.py 纯函数行为（语言归一、词级→segments 切分）——fastapi 可导入时执行
"""

from __future__ import annotations

import py_compile
import sys
import tomllib
import unittest
from pathlib import Path

MODULE_DIR = Path(__file__).resolve().parent.parent


class TestModuleToml(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(MODULE_DIR / "module.toml", "rb") as f:
            cls.manifest = tomllib.load(f)

    def test_module_basics(self):
        m = self.manifest["module"]
        self.assertEqual(m["id"], MODULE_DIR.name)
        self.assertEqual(m["category"], "asr")
        self.assertEqual(m["genre"], "qwen-asr")
        self.assertEqual(m["license"], "Apache-2.0")
        self.assertTrue(m["homepage"].startswith("https://github.com/QwenLM/Qwen3-ASR"))

    def test_runtime_and_compute(self):
        rt = self.manifest["runtime"]
        self.assertEqual(rt["type"], "python")
        self.assertIn("start_command", rt)
        self.assertIn("{venv_python}", rt["start_command"])
        compute = self.manifest["compute"]
        self.assertEqual(compute["backends"], ["cuda", "cpu"])
        self.assertEqual(compute["default_backend"], "cuda")
        self.assertGreater(compute["vram_estimate_mb"], 0)

    def test_models_three_variants(self):
        models = {m["id"]: m for m in self.manifest["models"]}
        self.assertEqual(
            set(models),
            {"qwen3-asr-0.6b", "qwen3-asr-1.7b", "qwen3-forced-aligner-0.6b"},
        )
        defaults = [mid for mid, m in models.items() if m.get("default")]
        self.assertEqual(defaults, ["qwen3-asr-0.6b"])
        expected_repos = {
            "qwen3-asr-0.6b": ("Qwen/Qwen3-ASR-0.6B", "qwen3-asr-0.6b", 1850, 2560),
            "qwen3-asr-1.7b": ("Qwen/Qwen3-ASR-1.7B", "qwen3-asr-1.7b", 4550, 5632),
            "qwen3-forced-aligner-0.6b": (
                "Qwen/Qwen3-ForcedAligner-0.6B", "qwen3-forced-aligner-0.6b", 1850, 2560,
            ),
        }
        for mid, (repo_id, target_dir, size_mb, vram_mb) in expected_repos.items():
            m = models[mid]
            self.assertEqual(m["source"], "huggingface", msg=mid)
            self.assertEqual(m["repo_id"], repo_id, msg=mid)
            self.assertEqual(m["target_dir"], target_dir, msg=mid)
            self.assertGreaterEqual(m["size_estimate_mb"], size_mb - 1, msg=mid)
            self.assertGreaterEqual(m["vram_estimate_mb"], vram_mb - 1, msg=mid)
            # 双源镜像：ModelScope 同名仓库，source 与主源不同
            mirrors = m.get("mirrors", [])
            self.assertTrue(mirrors, msg=f"{mid} 缺少镜像源")
            self.assertTrue(
                any(
                    mi["source"] == "modelscope" and mi["repo_id"] == repo_id
                    for mi in mirrors
                ),
                msg=mid,
            )
        # vram 排序合理性：1.7B > 0.6B 变体
        self.assertGreater(models["qwen3-asr-1.7b"]["vram_estimate_mb"],
                           models["qwen3-asr-0.6b"]["vram_estimate_mb"])

    def test_capabilities(self):
        caps = {c["name"]: c for c in self.manifest["interface"]["capabilities"]}
        self.assertEqual(set(caps), {"transcribe", "align"})

        tr = caps["transcribe"]
        self.assertEqual(tr["input_type"], "audio")
        self.assertEqual(tr["output_type"], "json")
        params = tr["params"]
        self.assertEqual(params["language"]["default"], "auto")
        self.assertEqual(params["context"]["type"], "string")
        self.assertIn("timestamps", params)

        al = caps["align"]
        self.assertEqual(al["input_type"], "audio")
        self.assertEqual(al["output_type"], "json")
        self.assertIn("text", al["params"])
        self.assertIn("language", al["params"])


class TestRequirements(unittest.TestCase):
    def setUp(self):
        self.lines = (MODULE_DIR / "requirements.txt").read_text(encoding="utf-8").splitlines()

    def _contains(self, prefix):
        return any(line.strip().startswith(prefix) for line in self.lines)

    def test_adapter_deps(self):
        for dep in ("fastapi>=0.100.0", "uvicorn[standard]>=0.23.0", "python-multipart>=0.0.6"):
            self.assertTrue(self._contains(dep), msg=dep)

    def test_qwen_asr_and_torch_pin(self):
        self.assertTrue(self._contains("qwen-asr>="))
        self.assertTrue(self._contains("torch==2.11.0"), "torch 需锁 constraints 同款 2.11.0")
        self.assertTrue(
            any("--extra-index-url https://download.pytorch.org/whl/cu130" in l for l in self.lines),
            "需内联 cu130 wheel 索引（constraints 不承载索引行）",
        )
        self.assertTrue(self._contains("transformers>="))
        self.assertTrue(self._contains("soundfile>="))


class TestAdapterCompiles(unittest.TestCase):
    def test_py_compile(self):
        py_compile.compile(str(MODULE_DIR / "adapter.py"), doraise=True)


class TestAdapterPureFunctions(unittest.TestCase):
    """纯函数验证；fastapi/qwen_asr 未安装时整体跳过。"""

    def setUp(self):
        try:
            import fastapi  # noqa: F401
            import uvicorn  # noqa: F401
        except ImportError:
            self.skipTest("fastapi/uvicorn 未安装（venv 构建后重跑）")
        sys.path.insert(0, str(MODULE_DIR))
        for mod in [m for m in list(sys.modules) if m == "adapter"]:
            del sys.modules[mod]
        import adapter

        self.adapter = adapter

    def test_normalize_language(self):
        f = self.adapter._normalize_language
        self.assertIsNone(f(None))
        self.assertIsNone(f(""))
        self.assertIsNone(f("auto"))
        self.assertEqual(f("zh"), "Chinese")
        self.assertEqual(f("EN"), "English")
        self.assertEqual(f("ja"), "Japanese")
        self.assertEqual(f("Chinese"), "Chinese")

    def test_segments_from_words(self):
        class Item:
            def __init__(self, text, s, e):
                self.text, self.start_time, self.end_time = text, s, e

        text = "你好世界。这是一个测试"
        items = [Item(t, float(i), float(i + 1)) for i, t in enumerate("你好世界这是一个测试")]
        segs = self.adapter._segments_from_words(text, items)
        self.assertEqual(len(segs), 2)
        self.assertEqual(segs[0]["text"], "你好世界。")
        self.assertAlmostEqual(segs[0]["end"] - segs[0]["start"], 4.0)
        self.assertEqual(segs[1]["text"], "这是一个测试")
        self.assertLess(segs[0]["end"], segs[1]["start"])

    def test_items_to_words_shape(self):
        class Item:
            def __init__(self):
                self.text, self.start_time, self.end_time = "hello", 1.23456, 2.0

        out = self.adapter._items_to_words([Item()])
        self.assertEqual(out[0], {"word": "hello", "start": 1.235, "end": 2.0})


if __name__ == "__main__":
    unittest.main(verbosity=2)
