"""ONNX Runtime Execution Provider 解析（纯函数，仅标准库）。

EP_BACKEND 词表与 MODULE_SPEC §4 环境变量契约一致：cuda / rocm / openvino / cpu。
返回的 providers 列表始终以 CPUExecutionProvider 兜底收尾——ORT 多 EP 语义下，
前序 provider 缺失或模型含不支持算子时自动回落 CPU，推理服务不致整体不可用。

provider options 键名依据（2026-08-22 调研）：
- CUDA / ROCm EP：选项键 ``device_id``
  （onnxruntime.ai/docs/execution-providers/CUDA|ROCm-ExecutionProvider）；
- OpenVINO EP：选项键 ``device_type``
  （onnxruntime.ai/docs/execution-providers/OpenVINO-ExecutionProvider，
  Configuration Options 表；Python API 形态
  providers=["OpenVINOExecutionProvider"], provider_options=[{...}]，
  等价的元组形态 [(name, options), ...] 由 InferenceSession 直接支持）。

OPENVINO_DEVICE 值域（即 device_type 允许值，ORT-OV ≥1.22 文档口径）：
    CPU | GPU | GPU.<n> | NPU | NPU.<n>
    AUTO:<d1,d2,...> | HETERO:<d1,d2,...> | MULTI:<d1,d2,...>
平台经 module.toml [compute.env] 注入 OPENVINO_DEVICE={device_name}
（取值如 GPU.0 / NPU.0 / CPU）。未注入时回退 "CPU"：确定性优先，避免 AUTO
在多设备机器上静默选卡。旧复合值（CPU_FP32/GPU_FP16 等）自 ORT 1.23 起废弃，
不再支持，勿用。

真机兼容注记（2026-08-22，Arrow Lake / OV 2025.4.1 实测）：单 NPU 机器上
OpenVINO 枚举名为裸 ``NPU``，``NPU.0`` 索引形态会被拒（"Device NPU.0 is not
available"）。故注入值匹配 ``NPU.<n>`` 时归一化为裸 ``NPU`` 再下发；
``GPU.<n>`` 索引形态实测有效，保持原样。
"""

from __future__ import annotations

import os
import re
from typing import Dict, Iterable, List, Mapping, Optional, Tuple, Union

VALID_BACKENDS: Tuple[str, ...] = ("cuda", "rocm", "openvino", "cpu")

# OPENVINO_DEVICE 未注入时的确定性回退（值域见模块 docstring）。
_DEFAULT_OV_DEVICE = "CPU"

# 单 NPU 机器的索引后缀归一（依据见模块 docstring 真机兼容注记）。
_NPU_INDEX_RE = re.compile(r"^NPU\.\d+$")

# 单条 provider 规格：裸名称，或 (名称, options 字典)。options 值一律字符串
# （ORT 内部按字符串传递；device_id/device_type 均如此）。
ProviderSpec = Union[str, Tuple[str, Dict[str, str]]]


def normalize_backend(value: Optional[str]) -> str:
    """归一化 EP_BACKEND：大小写/空白宽容；空值视为 cpu；未知值显式报错。

    宁可在启动时大声失败，也不把拼错的 backend 静默降级成 CPU——那会制造
    "账本记 GPU、实际跑 CPU" 的诚实性问题（rembg 曾因此返工）。
    """
    text = (value or "").strip().lower()
    if not text:
        return "cpu"
    if text not in VALID_BACKENDS:
        raise ValueError(
            f"Unsupported EP_BACKEND={value!r}; valid values: {', '.join(VALID_BACKENDS)}"
        )
    return text


def _device_index(env: Mapping[str, str]) -> str:
    """读取 EP_DEVICE_INDEX 并归一为规范十进制字符串（非法值显式报错）。"""
    raw = (env.get("EP_DEVICE_INDEX") or "0").strip() or "0"
    try:
        return str(int(raw))
    except ValueError:
        raise ValueError(f"EP_DEVICE_INDEX must be an integer, got {raw!r}") from None


def resolve_providers(
    ep_backend: Optional[str],
    env: Optional[Mapping[str, str]] = None,
) -> List[ProviderSpec]:
    """EP_BACKEND(+设备环境) → ORT providers 列表；列表恒以 CPUExecutionProvider 收尾。

    映射规则：
      cpu/缺省  -> ["CPUExecutionProvider"]
      cuda     -> [("CUDAExecutionProvider", {"device_id": <EP_DEVICE_INDEX>}), "CPUExecutionProvider"]
      rocm     -> [("ROCMExecutionProvider", {"device_id": <EP_DEVICE_INDEX>}), "CPUExecutionProvider"]
      openvino -> [("OpenVINOExecutionProvider", {"device_type": <OPENVINO_DEVICE 或 CPU>}),
                    "CPUExecutionProvider"]

    :param env: 环境变量映射（测试可传入假 env）；None 表示读 os.environ 以外的空映射。
    """
    env = env if env is not None else {}
    backend = normalize_backend(ep_backend)
    if backend == "cuda":
        return [
            ("CUDAExecutionProvider", {"device_id": _device_index(env)}),
            "CPUExecutionProvider",
        ]
    if backend == "rocm":
        return [
            ("ROCMExecutionProvider", {"device_id": _device_index(env)}),
            "CPUExecutionProvider",
        ]
    if backend == "openvino":
        device_type = (env.get("OPENVINO_DEVICE") or "").strip() or _DEFAULT_OV_DEVICE
        if _NPU_INDEX_RE.match(device_type):
            device_type = "NPU"
        return [
            ("OpenVINOExecutionProvider", {"device_type": device_type}),
            "CPUExecutionProvider",
        ]
    return ["CPUExecutionProvider"]


def provider_names(specs: Iterable[ProviderSpec]) -> List[str]:
    """投影出 provider 名称序列（日志与 /info 展示用）。"""
    return [item[0] if isinstance(item, tuple) else item for item in specs]


def resolve_providers_from_env() -> List[ProviderSpec]:
    """进程环境便捷入口：读 EP_BACKEND / OPENVINO_DEVICE / EP_DEVICE_INDEX。"""
    return resolve_providers(os.getenv("EP_BACKEND"), os.environ)
