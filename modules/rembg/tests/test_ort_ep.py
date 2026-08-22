"""ort_ep provider 映射纯函数单测（仅标准库，系统 python 可跑）。

覆盖 EP_BACKEND 三态及边界：cuda / openvino / 缺省-cpu，外加 rocm、
未知值报错、CPU 恒兜底收尾等断言；env 注入经 unittest.mock.patch.dict
（monkeypatch os.environ）完成。另含与 onnx-matting 副本的字节同步校验。
"""

import hashlib
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import ort_ep  # noqa: E402

# 兄弟模块副本（只读交叉校验：两份实现必须保持一致）
_SIBLING_ORT_EP = (
    Path(__file__).resolve().parents[2] / "onnx-matting" / "ort_ep.py"
)


class ResolveProvidersTest(unittest.TestCase):
    def test_default_is_cpu_only(self):
        with mock.patch.dict("os.environ", {}, clear=True):
            specs = ort_ep.resolve_providers_from_env()
        self.assertEqual(specs, ["CPUExecutionProvider"])

    def test_explicit_cpu(self):
        self.assertEqual(ort_ep.resolve_providers("cpu", {}), ["CPUExecutionProvider"])

    def test_openvino_device_type_injected(self):
        # 键名依据：onnxruntime.ai OpenVINO-ExecutionProvider 文档（device_type）
        specs = ort_ep.resolve_providers(
            "openvino", {"OPENVINO_DEVICE": "GPU.0"}
        )
        self.assertEqual(
            specs,
            [("OpenVINOExecutionProvider", {"device_type": "GPU.0"}), "CPUExecutionProvider"],
        )

    def test_openvino_npu_device(self):
        # 真机实证（2026-08-22，OV 2025.4.1）：单 NPU 枚举名为裸 NPU，
        # "NPU.0" 索引形态被拒——归一化后再下发（见 ort_ep 模块 docstring）。
        with mock.patch.dict(
            "os.environ", {"EP_BACKEND": "openvino", "OPENVINO_DEVICE": "NPU.0"}, clear=False
        ):
            specs = ort_ep.resolve_providers_from_env()
        self.assertEqual(specs[0], ("OpenVINOExecutionProvider", {"device_type": "NPU"}))

    def test_openvino_gpu_index_passthrough(self):
        # GPU.<n> 索引形态真机有效，保持原样
        with mock.patch.dict(
            "os.environ", {"EP_BACKEND": "openvino", "OPENVINO_DEVICE": "GPU.0"}, clear=False
        ):
            specs = ort_ep.resolve_providers_from_env()
        self.assertEqual(specs[0], ("OpenVINOExecutionProvider", {"device_type": "GPU.0"}))

    def test_openvino_without_device_falls_back_cpu(self):
        specs = ort_ep.resolve_providers("openvino", {})
        self.assertEqual(
            specs,
            [("OpenVINOExecutionProvider", {"device_type": "CPU"}), "CPUExecutionProvider"],
        )

    def test_cuda_device_id_from_env(self):
        with mock.patch.dict(
            "os.environ", {"EP_BACKEND": "CUDA", "EP_DEVICE_INDEX": "1"}, clear=False
        ):
            specs = ort_ep.resolve_providers_from_env()
        self.assertEqual(
            specs,
            [("CUDAExecutionProvider", {"device_id": "1"}), "CPUExecutionProvider"],
        )

    def test_cuda_default_index_zero(self):
        specs = ort_ep.resolve_providers("cuda", {})
        self.assertEqual(specs[0], ("CUDAExecutionProvider", {"device_id": "0"}))

    def test_rocm(self):
        specs = ort_ep.resolve_providers("rocm", {"EP_DEVICE_INDEX": "0"})
        self.assertEqual(
            specs, [("ROCMExecutionProvider", {"device_id": "0"}), "CPUExecutionProvider"]
        )

    def test_backend_case_and_whitespace_tolerant(self):
        names = ort_ep.provider_names(ort_ep.resolve_providers("  OpenVINO ", {}))
        self.assertEqual(names, ["OpenVINOExecutionProvider", "CPUExecutionProvider"])
        self.assertEqual(ort_ep.normalize_backend(None), "cpu")
        self.assertEqual(ort_ep.normalize_backend(""), "cpu")

    def test_unknown_backend_raises_loudly(self):
        # 不静默降级 CPU：拼错后端必须启动即失败，避免账本/执行不一致
        with self.assertRaises(ValueError):
            ort_ep.resolve_providers("directml", {})

    def test_bad_device_index_raises_loudly(self):
        with self.assertRaises(ValueError):
            ort_ep.resolve_providers("cuda", {"EP_DEVICE_INDEX": "gpu"})

    def test_cpu_provider_always_last(self):
        cases = [
            ("cpu", {}),
            ("cpu", None),
            ("openvino", {"OPENVINO_DEVICE": "GPU.0"}),
            ("openvino", {"OPENVINO_DEVICE": "NPU.0"}),
            ("cuda", {"EP_DEVICE_INDEX": "3"}),
            ("rocm", {"EP_DEVICE_INDEX": "1"}),
        ]
        for backend, env in cases:
            with self.subTest(backend=backend):
                self.assertEqual(ort_ep.resolve_providers(backend, env)[-1], "CPUExecutionProvider")


class SiblingSyncTest(unittest.TestCase):
    @unittest.skipUnless(_SIBLING_ORT_EP.is_file(), "onnx-matting module not present yet")
    def test_onnx_matting_copy_is_byte_identical(self):
        mine = Path(__file__).resolve().parents[1] / "ort_ep.py"
        a = hashlib.sha256(mine.read_bytes()).hexdigest()
        b = hashlib.sha256(_SIBLING_ORT_EP.read_bytes()).hexdigest()
        self.assertEqual(a, b, "rembg/onnx-matting 的 ort_ep.py 必须保持字节一致")


if __name__ == "__main__":
    unittest.main()
