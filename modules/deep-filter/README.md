# DeepFilter 音频降噪模块

基于 [DeepFilterNet](https://github.com/Rikorose/DeepFilterNet) 的深度学习语音增强/降噪模块。

## 功能

- AI 语音降噪（DeepFilterNet3 模型）
- 支持 CUDA / CPU 推理
- 可调节降噪强度与最小增益
- 当 DeepFilterNet 不可用时自动回退到 scipy 频谱减法

## 接口

| 端点 | 方法 | 说明 |
|------|------|------|
| `/health` | GET | 健康检查（模型未就绪返回 503） |
| `/info` | GET | 模块信息 |
| `/predict/denoise` | POST | 音频降噪 |

### POST /predict/denoise

支持两种输入方式：

1. **JSON body** — `{"input_path": "/path/to/audio.wav"}`
2. **Multipart file** — 上传 `file` 字段

可选参数：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `attenuation` | int | 100 | 降噪强度 (dB)，0–100 |
| `min_db` | float | -60.0 | 最小增益 (dB)，-100–0 |

响应示例：

```json
{
  "status": "ok",
  "output_path": "/workspace/denoised_xxxx.wav",
  "backend": "deepfilternet",
  "sample_rate": 48000,
  "duration_secs": 12.5
}
```

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `EP_PORT` | 服务端口 | 8900 |
| `EP_MODEL_DIR` | 模型目录 | `.` |
| `EP_DEVICE` | 设备索引 | 0 |
| `EP_BACKEND` | 推理后端 | cpu |
| `EP_WORKSPACE` | 工作/输出目录 | 系统临时目录 |
| `EP_MODULE_ID` | 模块实例 ID | deep-filter |

## 本地运行

```bash
pip install -r requirements.txt
python adapter.py
```
