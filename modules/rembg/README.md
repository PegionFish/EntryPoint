# RemBG 智能去背景模块

基于 [rembg](https://github.com/danielgatis/rembg) 的 AI 图像背景移除模块，支持 U2-Net / ISNet / BiRefNet 三种模型。

## 功能

- **remove_bg** — 移除图片背景，输出透明 PNG
  - 支持 multipart 文件上传或服务器端路径输入
  - 可选 alpha matting 精细边缘
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
| `EP_MODEL_NAME` | `u2net` | 默认模型 |
| `EP_DEVICE_INDEX` | `0` | GPU 设备索引 |
| `EP_LOG_LEVEL` | `INFO` | 日志级别 |

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
| `model` | string | 否 | 模型名（u2net / isnet-general-use / birefnet-general） |
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

| 模型 | 大小 | 说明 |
|------|------|------|
| u2net | ~176 MB | 通用，默认 |
| isnet-general-use | ~176 MB | 高精度 |
| birefnet-general | ~880 MB | 最新，效果最佳 |

## 许可

MIT
