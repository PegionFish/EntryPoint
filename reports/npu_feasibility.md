# 本机 Intel AI Boost NPU OpenVINO 推理可行性实证报告

> 探测日期：2026-08-05 ｜ 探测代理：NPU-PROBE（多代理并行验证作业）
> 探测环境：`runtime/venvs/openvino-probe/`（CPython 3.12.13 + openvino **2026.3.0**-22451 + onnx 1.22.0 + numpy 2.5.1，uv 0.11.32 直连 pypi.org 安装）
> 结论一句话：**NPU 可用（可推理）**——三种规模合成模型在 NPU 上全部编译成功、推理输出正确、执行归因确凿（`EXECUTION_DEVICES: NPU`），但本机 CPU 极强（270HX Plus），小模型场景 NPU 稳态延迟并不占优，价值在卸载 CPU 与并行编排。

---

## 1. 硬件与驱动环境

| 项 | 值 | 来源 |
|---|---|---|
| CPU | Intel Core Ultra 7 270K Plus（营销名，FULL_DEVICE_NAME 实测） | OpenVINO CPU 插件 |
| iGPU | Intel(R) Graphics，uarch v12.70.4，64 EU，~33.6 GiB 共享显存，驱动 32.0.101.6129 | GPU.0 属性 + Win32_VideoController |
| dGPU | NVIDIA GeForce RTX 5090 D，驱动 32.0.16.1088（**OpenVINO GPU 插件亦枚举，见 §2.2**） | GPU.1 属性 |
| NPU | Intel(R) AI Boost，平台码 3720，2 tiles，DEVICE_TYPE=INTEGRATED | NPU 插件属性 |
| NPU 驱动版本 | `NPU_DRIVER_VERSION: 1004778`（对应 Windows 驱动 32.0.100.4778 系列；PnP/CIM 路径查不到 NPU DriverVersion，插件属性为唯一权威源） | NPU 插件属性 |
| NPU 编译器版本 | `NPU_COMPILER_VERSION: 524289` | NPU 插件属性 |
| NPU 标称算力 | FP16 6553.6 GOPS / INT8 13107.2 GOPS（≈13 TOPS，与 AI Boost 标称一致）；FP32/BF16 = 0 | DEVICE_GOPS |
| NPU 能力 | OPTIMIZATION_CAPABILITIES: FP16, INT8, EXPORT_IMPORT；异步请求区间 (1,8,1)；INFERENCE_PRECISION_HINT=float16 | 属性实测 |
| NPU 内存 | `NPU_DEVICE_TOTAL_MEM_SIZE: 34359738368`（32 GiB——共享系统内存口径，非专属显存；占用量无直接查询渠道） | 属性实测 |

设备管理器侧交叉验证：`Win32_PnPEntity` 匹配到 **"Intel(R) AI Boost"，Status=OK**（与 ep-core PnP 兜底同名同源）。

## 2. 设备枚举

### 2.1 原始输出

```text
python: 3.12.13
openvino: 2026.3.0-22451-8a17657b995-releases/2026/3
available_devices: ['CPU', 'GPU.0', 'GPU.1', 'NPU']

device: CPU
  FULL_DEVICE_NAME: Intel(R) Core(TM) Ultra 7 270K Plus
  DEVICE_TYPE: Type.INTEGRATED
  DEVICE_ARCHITECTURE: intel64
  OPTIMIZATION_CAPABILITIES: ['BF16', 'FP32', 'FP16', 'INT8', 'BIN', 'EXPORT_IMPORT']
  RANGE_FOR_STREAMS: (1, 24)

device: GPU.0
  FULL_DEVICE_NAME: Intel(R) Graphics (iGPU)
  DEVICE_TYPE: Type.INTEGRATED
  DEVICE_ARCHITECTURE: GPU: vendor=0x8086 arch=v12.70.4
  AVAILABLE_DEVICES: ['0', '1']
  OPTIMIZATION_CAPABILITIES: ['FP32', 'BIN', 'FP16', 'INT8', 'GPU_USM_MEMORY', 'EXPORT_IMPORT']
  DEVICE_GOPS: {float16: 4096.0, float32: 2048.0, int8: 8192.0, uint8: 8192.0}
  GPU_DEVICE_TOTAL_MEM_SIZE: 36037804032
  GPU_EXECUTION_UNITS_COUNT: 64

device: GPU.1
  FULL_DEVICE_NAME: NVIDIA GeForce RTX 5090 D (dGPU)
  DEVICE_TYPE: Type.DISCRETE
  DEVICE_ARCHITECTURE: GPU: vendor=0x10de arch=v12.0.0
  DEVICE_GOPS: {float16: 0.0, float32: 0.0, int8: 0.0, uint8: 0.0}   ← 查询口径异常，但可编译可推理，见 §2.2
  GPU_DEVICE_TOTAL_MEM_SIZE: 34190458880

device: NPU
  FULL_DEVICE_NAME: Intel(R) AI Boost
  DEVICE_TYPE: Type.INTEGRATED
  DEVICE_ARCHITECTURE: 3720
  AVAILABLE_DEVICES: ['3720']
  OPTIMIZATION_CAPABILITIES: ['FP16', 'INT8', 'EXPORT_IMPORT']
  DEVICE_GOPS: {bfloat16: 0.0, float16: 6553.6, float32: 0.0, int8: 13107.2, uint8: 13107.2}
  NPU_DRIVER_VERSION: 1004778
  NPU_COMPILER_VERSION: 524289
  NPU_MAX_TILES: 2
  NPU_PLATFORM: AUTO_DETECT
  RANGE_FOR_ASYNC_INFER_REQUESTS: (1, 8, 1)
  RANGE_FOR_STREAMS: (0, 8)
  INFERENCE_PRECISION_HINT: <Type: 'float16'>
  OPTIMAL_NUMBER_OF_INFER_REQUESTS: 1
```

（NPU 完整 SUPPORTED_PROPERTIES 含 NPU_TURBO / NPU_QDQ_OPTIMIZATION / CACHE_DIR / WORKLOAD_TYPE /
NPU_COMPILATION_MODE_PARAMS 等可调项，探测脚本留档于 `runtime/venvs/openvino-probe/probe_devices.py`。）

不支持属性的报错示例（如实记录）：CPU/GPU 的 `DEVICE_SUBGROUP_SIZES` 报
`Property DEVICE_SUBGROUP_SIZES is not in a list of supported properties`；
NPU 侧报 `Unsupported configuration key: DEVICE_SUBGROUP_SIZES`。均正常跳过，不影响推理。

### 2.2 意外发现：GPU.1 是 NVIDIA 卡，且 OpenVINO 2026.3 真能跑

OpenVINO 2026.3 的 GPU 插件已枚举 NVIDIA dGPU（vendor=0x10de）并**实测编译、推理成功**
（小模型 compile 0.374~0.998s，稳态 ~0.5ms）。`DEVICE_GOPS` 全 0 只是能力查询口径异常，
不代表不可执行。这属于 OpenVINO 新近的实验性 NVIDIA 支持通道。

**对 ep-core 的影响（仅提示，不在本任务改动范围）**：

- 默认检测路径**安全**：`gpu_fallback_devices()` 按名称过滤只留 Intel（`openvino.rs` 注释"NVIDIA/AMD
  分别归 CUDA/ROCm 语义"），本机只会产出 `openvino:GPU.0`（Intel iGPU）；xpu-smi 工具本身也只枚举
  Intel GPU，无泄漏。
- **泄漏点在可选 Python 探测路径**：`EP_OPENVINO_PYTHON_PROBE=1` 时 `parse_openvino_python_probe()`
  只跳过 CPU，不按 vendor 过滤 → NVIDIA 会被追加为 `openvino:GPU.1`，与 CUDA 检测器的枚举重复。
  建议后续给该解析函数加 Intel 过滤（vendor 0x8086 / 名称含 Intel）。

### 2.3 与 ep-core `openvino.rs` 兜底检测的命名对照

本机无 intel-npu-smi / xpu-smi，ep-core 走 PnP Win32 兜底，报 `openvino:NPU.0` / `openvino:GPU.0`。对照：

| ep-core 设备 ID | ep-core 产出路径 | OpenVINO 运行时对应 | 一致性 |
|---|---|---|---|
| `openvino:NPU.0` | PnP 名 "Intel(R) AI Boost" 命中 → `NPU.{seq=0}` | `available_devices` 中的 `NPU`（Python 探测解析规则：无编号 NPU 补 `.0`） | ✅ 一致 |
| `openvino:GPU.0` | Win32_VideoController Intel 过滤 → `GPU.{seq=0}`（"Intel(R) Graphics"） | `GPU.0` = Intel(R) Graphics (iGPU) | ✅ 一致 |
| —（不产出） | Intel 过滤排除 NVIDIA | `GPU.1` = NVIDIA RTX 5090 D（仅 python-probe 路径会引入，见 §2.2） | ⚠️ 见 §2.2 建议 |

命名映射结论：**兜底检测与 OpenVINO 运行时设备一一对应，无错配**。`.0` 序号语义两侧都是
"同 kind 内的出现次序"，本机单 NPU / 单 Intel GPU 下恒为 0，天然对齐。

## 3. 推理基准

### 3.1 模型与方法

onnx.helper 手写三档静态 shape 合成模型（opset 18），每设备 `compile_model` + 连续 10 次 `infer`，
首次为热启动（含设备初始化/权重上传），稳态取后 9 次均值。探测脚本与产物留档于
`runtime/venvs/openvino-probe/`（bench_main / bench_medium / bench_heavy / bench_variants，gitignore 内）。

- **small**：1x3x224x224 → Conv3x3(3→16) → Relu → Conv3x3(16→32,s2) → Relu → ReduceMean → Flatten → MatMul(32→10) → Softmax（~0.02M 参数）
- **medium**：1x3x320x320（u2net 输入同规格）→ Conv3x3×8 @64ch（stride2×3）→ Conv1x1→1 → Sigmoid（0.26M 参数，分割掩码形态）
- **heavy**：1x3x320x320 → Conv3x3×8 通道 64→64→128→128→256→256→256→256（stride2×3）→ Conv1x1→1 → Sigmoid（2.33M 参数，接近 u2net 编码器规模）

### 3.2 延迟数据表

编译耗时：

| 模型 | CPU | GPU.0 (iGPU) | NPU |
|---|---|---|---|
| small | 0.025 s | 2.042 s | **0.409 s** |
| medium | 0.031 s | 1.339 s | **0.268 s** |
| heavy | 0.031 s | 1.416 s | **0.403 s** |
| small（FP16 压缩 IR） | — | — | **0.124 s** |

推理延迟（10 次 infer）：

| 模型 | 设备 | 首次（热启动） | 稳态均值（后 9 次） | min | max |
|---|---|---|---|---|---|
| small | CPU | 1.63 ms | 0.48 ms | 0.32 | 1.01 |
| small | GPU.0 | 0.93 ms | 0.53 ms | 0.51 | 0.59 |
| small | **NPU** | **189.53 ms** | **1.34 ms** | 1.03 | 2.76 |
| small（FP16 IR） | **NPU** | 467.42 ms | **1.04 ms** | 1.00 | 1.08 |
| medium | CPU | 11.98 ms | 7.20 ms | 6.55 | 8.69 |
| medium | GPU.0 | 6.70 ms | 6.00 ms | 5.84 | 6.15 |
| medium | **NPU** | **297.21 ms** | **12.89 ms** | 11.75 | 13.89 |
| heavy | CPU | 21.47 ms | 18.10 ms | 16.43 | 20.70 |
| heavy | GPU.0 | 13.10 ms | 12.95 ms | 12.55 | 13.57 |
| heavy | **NPU** | **186.36 ms** | **21.49 ms** | 18.66 | 25.53 |

参考（§2.2 发现）：GPU.1（NVIDIA RTX 5090 D）跑 small：compile 0.374~0.998 s，稳态 ~0.50 ms（实验性通道）。

输出正确性：softmax 输出和 = 1.000000~1.000305（FP16 数值噪声内）；sigmoid 掩码均值 ≈0.4998~0.5034，
三设备间一致。执行归因：编译产物 `EXECUTION_DEVICES` 属性实测为 `NPU`，**无静默回退 CPU**。

### 3.3 NPU 编译失败记录

**无。** 三档模型（FP32 ONNX 与 FP16 压缩 IR 两条路径）在 NPU 上全部一次编译通过，
未触发 NHWC 布局调整或算子降级——2026.3 NPU 插件对 NCHW 静态 shape 小模型无布局要求。

探测过程中遇到的**工具链**坑（非 NPU 问题，但对后续 adapter 开发有价值，如实记录）：

1. `onnx.checker` 报 `No Op registered for GlobalAveragePooling with domain_version of 18`——
   系探测脚本误用 PyTorch 风格算子名，ONNX 标准名为 `GlobalAveragePool`；改用等价的
   `ReduceMean(axes=[2,3], keepdims=1)` 规避。
2. openvino 2026.3 的 `ov.convert_model()` 只接受**文件路径**，传内存中 `onnx.ModelProto` 报
   `Unknown model type`；需先 `onnx.save_model` 落盘。
3. 顶层 `ov.read_model` 已移除（`AttributeError`），需用 `ov.Core().read_model()`。

## 4. 结论与建议

### 4.1 可用性评级：**可推理**

- 编译：三规模 × FP32/FP16 全通过，NPU 编译极快（≤0.41s，快于 iGPU 插件 3~5 倍）。
- 执行：归因确凿，输出数值正确。
- 成本：首次推理含 186~467ms 设备初始化（进程内一次性开销，长驻 adapter 模型下摊薄可接受）。

**延迟定位（本机口径）**：270HX Plus 的 CPU 极强（24 流），小/中模型上 NPU 稳态延迟与 CPU 相当、
略慢于 iGPU；NPU 价值不在裸延迟，而在**卸载 CPU、释放并发头部空间与能效**——对模块编排平台
（rembg 跑 NPU 时 CPU 留给 faster-whisper 解码等并行负载）正是对口场景。轻薄本（U/H 系）上
NPU 相对优势会比本机（HX 系）更明显。

### 4.2 是否值得为 rembg（ONNX u2net 系）排期 openvino 后端

**值得，建议中低优先级（实验性后端）排期。** 依据：

- u2net 系 = 卷积为主 + 静态 320x320 输入 + ONNX 原生格式，与本探测验证过的 NPU 算子面高度吻合。
- 本机实证 NPU 编译快、推理稳、FP16 路径可用（u2net FP16 压缩后 ~88MB，NPU 原生 FP16 数据通路）。
- 风险项：本探测为合成模型，**未跑真实 u2net ONNX**（其 MaxPool/Concat/Resize 等算子未覆盖），
  排期第一步应做真实模型 NPU 冒烟（预计无障碍，但需实证）；INT8 量化路径（NNCF）未测，
  留作二期提速手段（INT8 GOPS 为 FP16 两倍）。

### 4.3 最小改造路径建议（供后续实施代理参考）

1. **module.toml**：rembg（及后续候选模块）`backends` 追加 `openvino`；设备选择复用现有
   `openvino:NPU.0` / `openvino:GPU.0` 配置通道（§2.3 已证命名对齐）。
2. **adapter 实现取舍：推荐 openvino 直接推理，而非 onnxruntime-openvino EP**：
   - openvino 直接：`ov.convert_model(onnx_path)`（或发行侧预转 IR + `compress_to_fp16`）→
     `compile_model(model, "NPU")`；可完整控制 NPU 属性（`CACHE_DIR` 编译缓存、`NPU_TURBO`、
     `WORKLOAD_TYPE`、异步请求数 ≤8），版本与本机插件严格同步，无 ORT 版本钳制问题。
   - ORT EP 路线仅在模块 adapter 已深度绑定 onnxruntime 时作为省事选项；EP 对新 NPU 特性的
     支持通常滞后，排错多一层间接。
3. **必做工程项**：compile 后立即 warmup 一次推理（吸收 186~467ms 初始化，避免首帧毛刺）；
   设置 `CACHE_DIR` 持久化编译缓存；Python 侧注意 §3.3 三个 API 口径坑。
4. **设备路由注意**：NVIDIA 卡不走 openvino 后端（归 CUDA）；若启用 `EP_OPENVINO_PYTHON_PROBE=1`，
   先落地 §2.2 的 Intel vendor 过滤修复，避免 `openvino:GPU.1` 幽灵设备。

### 4.4 未覆盖项（遗留）

- 真实 u2net ONNX 冒烟（需 ~170MB 模型下载，本次未做）。
- INT8/NNCF 量化、`NPU_TURBO`、多流（NUM_STREAMS）、多异步请求并发压测。
- NPU 显存/占用监控手段（插件无直接查询；`NPU_DEVICE_ALLOC_MEM_SIZE` 可作间接信号）。
- Linux 侧对照（本机仅 Windows）。
