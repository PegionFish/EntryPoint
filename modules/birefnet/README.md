# ONNX Matting 图像抠图模块

基于 [BiRefNet](https://github.com/ZhengPeng7/BiRefNet)（MIT，Tier A，见
`reports/license-matrix.md`）ONNX 权重的图像 alpha 抠图模块。ONNX Runtime
多 EP 直跑：cuda / rocm / openvino / cpu 同一份 ONNX 权重、同一 adapter，
按 `EP_BACKEND` 分派执行 provider（恒以 CPU 兜底收尾）。

## 功能

- **matte** — 图像 alpha 抠图，输出透明 PNG（保留原分辨率）
  - 预处理：resize 1024×1024 → /255 → ImageNet mean/std → NCHW float32（对齐官方推理脚本）
  - 输出：logits → sigmoid → alpha mask 上采样回原尺寸 → 与原图合成 RGBA PNG
  - 支持 multipart 文件上传或服务器端路径输入；支持管线产物协议（`params.output_path`）

## 快速开始

```bash
# 安装依赖（cpu 基础栈）
pip install -r requirements.txt

# 启动服务（默认端口 8901）
python adapter.py

# 冒烟
curl http://127.0.0.1:8901/health
curl -X POST http://127.0.0.1:8901/predict/matte -F "file=@photo.jpg"
```

## 计算后端

| backend | ORT 发行版 | provider options | 备注 |
|---|---|---|---|
| `cuda` | `onnxruntime-gpu` | `device_id = {EP_DEVICE_INDEX}` | 需主机 CUDA 12.x + cuDNN 9 |
| `rocm` | `onnxruntime-rocm` | `device_id = {EP_DEVICE_INDEX}` | **实验性**：仅 linux wheel，未真机验证（E1 先行） |
| `openvino` | `onnxruntime-openvino` | `device_type = {OPENVINO_DEVICE}` | Intel CPU/GPU/NPU；E2/E3 载体 |
| `cpu` | `onnxruntime` | — | 回退兜底，始终可用 |

- **ORT 发行版互斥**：`onnxruntime / -gpu / -rocm / -openvino` 提供同一个
  `onnxruntime` 包命名空间，**不可共存于同一 venv**。平台经
  `[runtime].requirements_by_backend`（M2）+ 分后端 venv `<module>--<backend>`（M3）
  按当前后端安装对应文件；手动部署时请为每个后端建独立 venv。
- **provider options 键名依据**：CUDA/ROCm 为 `device_id`；OpenVINO 为
  `device_type`（onnxruntime.ai Execution Provider 文档）。`OPENVINO_DEVICE`
  值域：`CPU | GPU | GPU.<n> | NPU | NPU.<n> | AUTO:<...> | HETERO:<...> | MULTI:<...>`，
  由平台注入 `{device_name}`（如 `GPU.0` / `NPU.0`）；缺省回退 `CPU`。
- openvino 后端会话同时关闭 ORT 图优化（OV EP 官方建议，图优化交给 OpenVINO）。

## 模型与权重约定

| 变体 id | target_dir | 约定权重文件名 | 显存估算 |
|---|---|---|---|
| `birefnet-general` | `birefnet-birefnet-general` | `birefnet-general.onnx` | ~1000 MB |
| `birefnet-portrait` | `birefnet-birefnet-portrait` | `birefnet-portrait.onnx` | ~972 MB |

查找顺序：`EP_MODELS_ROOT/<target_dir>/` → `EP_MODEL_DIR/`；目录内无约定名但
恰有一个 `*.onnx` 时按手动放置宽容接受。

### 权重获取

module.toml 以 `source=huggingface`（`ZhengPeng7/BiRefNet` /
`ZhengPeng7/BiRefNet-portrait`）声明上游出处——2026-08-22 核实上游仓库现只发布
torch/safetensors 权重、无官方 ONNX 可直链，故运行时消费的 ONNX 按上述命名约定放置：

1. **开发期（推荐）**：从本机 FaceFusion 包 `cp` 对应 ONNX（`birefnet_general.onnx`
   → `birefnet-general.onnx`、`birefnet_portrait.onnx` → `birefnet-portrait.onnx`，
   HETERO_DIST_PLAN §3.3 开发期权重使用纪律允许）；
2. **自转换**：clone 上游 repo，加载 safetensors 后按其 `inference.py` 导出 ONNX
   （输入 `1×3×1024×1024`，输出 logits 单通道）；
3. **通用变体的第三方转换件**：rembg 官方 Release 资产
   `https://github.com/danielgatis/rembg/releases/download/v0.0.0/BiRefNet-general-epoch_244.onnx`
   （重命名为 `birefnet-general.onnx` 放入 target_dir）。

权重缺失时 `/predict/matte` 返回 `503 MODEL_NOT_LOADED` 并附期望路径，绝不静默联网下载。

## API

### GET /health

健康检查（模型就绪前返回 503）。

### GET /info

模块元信息，含 `ep_backend` / `requested_providers` / `providers`
（session 实际激活 EP——E2/E3 设备验证观测点）。

### POST /predict/matte

**参数**（multipart form 或 JSON body）：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `file` | file | 二选一 | 上传图片文件 |
| `input_path` | string | 二选一 | 服务器端文件路径 |
| `params.model` | string | 否 | 变体覆盖（birefnet-general / birefnet-portrait） |

**响应**（契约同 rembg adapter）：`{status:"completed", output_type:"file",
result:<输出路径>, output_path:<输出路径>, metadata:{model,...}, elapsed_seconds}`。

## 许可

代码 MIT。模型权重 MIT（ZhengPeng7/BiRefNet，Tier A 可捆绑再分发；
分发时保留上游版权与许可声明）。
