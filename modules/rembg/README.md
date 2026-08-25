# RemBG 智能去背景模块

基于 [rembg](https://github.com/danielgatis/rembg) 的 AI 图像背景移除模块，
支持 U2-Net / ISNet / BiRefNet 共 7 个变体（6 个与源应用 RemBg 全量对齐）。

## 功能

- **remove_bg** — 移除图片背景，输出透明 PNG
  - 支持 multipart 文件上传或服务器端路径输入
  - 可选 alpha matting 精细边缘（仅 isnet-general-use / birefnet-general
    支持；其余模型传 true 时自动降级为 false，响应 metadata.warning 提示，绝不报错）
  - 可选后处理优化

## 快速开始

```bash
# 安装依赖
pip install -r requirements.txt

# 启动服务（默认端口 8900）
python adapter.py
```

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `EP_HOST` | `127.0.0.1` | 监听地址 |
| `EP_PORT` | `8900` | 监听端口 |
| `EP_WORKSPACE` | `./workspace` | 输出目录 |
| `EP_MODEL_ID` | `u2net` | 默认模型变体（daemon 注入，对应 module.toml `[[models]].id`；兼容旧键 `EP_MODEL_NAME` 作回退） |
| `EP_MODEL_DIR` | 空 | 模型目录（daemon 注入）；非空时映射为 rembg 的 `U2NET_HOME`，消费 daemon 预下载的 `<model>.onnx` |
| `EP_DEVICE_INDEX` | `0` | cuda/rocm 后端的设备索引（注入 provider option `device_id`） |
| `EP_BACKEND` | `cpu` | 计算后端：`cuda` / `rocm` / `openvino` / `cpu`（ORT providers 分派，恒以 CPU 兜底收尾；未知值启动即报错不静默降级） |
| `OPENVINO_DEVICE` | 空（回退 `CPU`） | openvino 后端目标设备，经 provider option 键 `device_type` 传入。值域：`CPU \| GPU \| GPU.<n> \| NPU \| NPU.<n> \| AUTO:<...> \| HETERO:<...> \| MULTI:<...>`；平台注入 `{device_name}`（如 `GPU.0` / `NPU.0`） |
| `EP_LOG_LEVEL` | `INFO` | 日志级别 |

## 计算后端（W1/WS-C 多后端化）

module.toml `[compute].backends = ["openvino", "cpu"]`：

- **openvino** — 依赖文件 `requirements-openvino.txt` 安装 Intel 的
  onnxruntime-openvino 整合 wheel（内置 OpenVINOExecutionProvider + CPUExecutionProvider），
  adapter 经 `new_session(providers=...)` 注入
  `[("OpenVINOExecutionProvider", {"device_type": <OPENVINO_DEVICE>}), "CPUExecutionProvider"]`；
  u2net/isnet ONNX 直接吃 EP、模型不变。E2（NPU.0）/E3（GPU.0）实验载体。
  该 wheel 与官方 onnxruntime 同包名互斥，故经 M2 `requirements_by_backend`
  分后端装包、M3 分后端 venv（`rembg--openvino`），不要与基础栈混装。
- **cpu** — 基础 `requirements.txt`（rembg[cpu] → 官方 onnxruntime），始终可用；
  当前 `default_backend = "cpu"`，E2/E3 真机验证通过后可上调。
- **cuda/rocm** — 本期不做（诚实声明）：ORT 各发行 wheel 单 venv 互斥，
  待分后端 venv 全量铺开后再评估 onnxruntime-gpu 路线，未验证不声明。

`GET /info` 返回 `ep_backend` / `requested_providers` / `providers`
（session 实际激活 EP），供 E2/E3 验证观测。

## API

### GET /health

健康检查，返回模型加载状态。

### GET /info

模块元信息。

### POST /predict/remove_bg

移除图片背景。

**参数**（multipart form）：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `file` | file | 二选一 | 上传图片文件 |
| `input_path` | string | 二选一 | 服务器端文件路径 |
| `model` | string | 否 | 模型名（u2net / u2netp / u2net_human_seg / u2net_cloth_seg / isnet-general-use / isnet-anime / birefnet-general） |
| `alpha_matting` | bool | 否 | 启用 alpha matting（默认 false） |
| `post_process` | bool | 否 | 后处理优化边缘（默认 true） |

**示例**：

```bash
curl -X POST http://127.0.0.1:8900/predict/remove_bg \
  -F "file=@photo.jpg" \
  -F "post_process=true"
```

**响应**：

```json
{
  "status": "ok",
  "output_path": "workspace/photo_a1b2c3d4.png",
  "model": "u2net",
  "alpha_matting": false,
  "post_process": true,
  "output_size_bytes": 1234567
}
```

## 模型

| 模型 | 大小 | 说明 | alpha matting |
|------|------|------|------|
| u2net | ~176 MB | 通用，默认 | 否（降级） |
| u2netp | ~5 MB | 轻量低耗 | 否（降级） |
| u2net_human_seg | ~176 MB | 人像分割 | 否（降级） |
| u2net_cloth_seg | ~176 MB | 服装分割 | 否（降级） |
| isnet-general-use | ~176 MB | 高精度 | 是 |
| isnet-anime | ~176 MB | 动漫图 | 否（降级） |
| birefnet-general | ~880 MB | 最新，效果最佳 | 是 |

> alpha matting：仅 general 系（isnet-general-use / birefnet-general）受支持；
> 其余模型请求 `alpha_matting=true` 时降级关闭，响应 `metadata.warning` 给出
> 说明（业务继续，不报错）。

## 许可

MIT
