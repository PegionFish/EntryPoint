#!/usr/bin/env python
"""export_onnx.py — RealESR 权重 → 动态 shape ONNX（E7：OV-ONNX 自建导出）

决策备忘录 reports/ws-f-engine-choice.md §1.2 裁定：上游无官方 ONNX 发布，
社区转换件输入固定 128px 不可用；按 Intel doc 816445 自转 dynamic-shape
ONNX（H/W 动轴）并经 PSNR 对拍核验后作为 openvino 后端权威权重。

用法（realesr torch venv 内运行，依赖 torch/basicsr 链）：
    python scripts/export_onnx.py <weights.pth> <output.onnx>

产物契约：
    - 输入 NCHW fp32 RGB [0,1]，动态 N/H/W（opset 17）
    - 与 torch 路线同源权重 strict 加载，保证数值一致性可对拍
"""
from __future__ import annotations

import sys
import types
from pathlib import Path

import torch

# basicsr 1.4.2 引用的 functional_tensor 在 torchvision>=0.17 已移除——
# 与 adapter torch 分支同款 shim（post-install 自检同源逻辑）
import torchvision.transforms.functional as _tvf

_shim = types.ModuleType("torchvision.transforms.functional_tensor")
_shim.rgb_to_grayscale = _tvf.rgb_to_grayscale
sys.modules.setdefault("torchvision.transforms.functional_tensor", _shim)

from realesrgan.archs.srvgg_arch import SRVGGNetCompact  # noqa: E402


def srvgg_preset(stem: str) -> tuple[int, int]:
    """架构预设表（与 adapter `_srvgg_preset` 同源，实证键集+形状全匹配）：
    animevideov3 → (16, 4)；xsx2 → (16, 2)；xsx4 → (32, 4)"""
    s = stem.lower()
    if "xsx4" in s:
        return 32, 4
    if "xsx2" in s:
        return 16, 2
    return 16, 4  # animevideov3 / 缺省主线轻量


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    weight = Path(sys.argv[1])
    out_path = Path(sys.argv[2])
    if not weight.is_file():
        print(f"weights not found: {weight}", file=sys.stderr)
        return 1

    num_conv, upscale = srvgg_preset(weight.stem)
    net = SRVGGNetCompact(
        num_in_ch=3, num_out_ch=3, num_feat=64,
        num_conv=num_conv, upscale=upscale, act_type="prelu",
    )
    ckpt = torch.load(str(weight), map_location="cpu", weights_only=False)
    state = ckpt.get("params_ema") or ckpt.get("params") or ckpt
    net.load_state_dict(state, strict=True)
    net.eval()

    dummy = torch.randn(1, 3, 64, 64)
    with torch.no_grad():
        torch.onnx.export(
            net,
            dummy,
            str(out_path),
            export_params=True,
            opset_version=17,
            do_constant_folding=True,
            input_names=["input"],
            output_names=["output"],
            dynamic_axes={
                "input": {0: "N", 2: "H", 3: "W"},
                "output": {0: "N", 2: "H", 3: "W"},
            },
        )
    size_mb = out_path.stat().st_size / (1024 * 1024)
    print(f"exported {out_path} ({size_mb:.1f} MB) preset=(num_conv={num_conv}, upscale={upscale})")

    # 导出自检：onnxruntime CPU 快速前向 + 尺寸断言
    import onnxruntime as ort

    sess = ort.InferenceSession(str(out_path), providers=["CPUExecutionProvider"])
    import numpy as np

    x = np.random.rand(1, 3, 90, 160).astype(np.float32)
    y = sess.run(None, {sess.get_inputs()[0].name: x})[0]
    assert y.shape == (1, 3, 360, 640), f"unexpected output shape {y.shape}"
    print(f"self-check OK: 90x160 -> {y.shape[3]}x{y.shape[2]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
