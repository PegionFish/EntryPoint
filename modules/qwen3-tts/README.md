# Qwen3-TTS 语音合成模块

基于 [Qwen3-TTS](https://github.com/QwenLM/Qwen3-TTS) 的高质量多语言语音合成模块，支持中英文等多种语言。

## 功能

- **文本转语音**：将输入文本合成为 WAV 音频文件
- **多模型支持**：1.7B（高质量）和 0.6B（轻量）两种模型可选
- **多后端**：支持 CUDA GPU 加速和 CPU 推理
- **Fallback**：当 Qwen3-TTS 模型不可用时，自动降级为 edge-tts 在线合成

## 快速开始

### 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `EP_MODULE_DIR` | 模块根目录 | 当前目录 |
| `EP_MODEL_DIR` | 模型文件目录 | `EP_MODULE_DIR/models` |
| `EP_WORKSPACE` | 输出工作目录 | 当前目录 |
| `EP_PORT` | HTTP 服务端口 | `8000` |
| `EP_DEVICE` | 计算后端 (`cuda` / `cpu`) | `cuda` |
| `EP_MODEL_ID` | 使用的模型 ID | `1.7b` |

### 启动服务

```bash
pip install -r requirements.txt
python adapter.py
```

### API

#### 健康检查

```
GET /health
```

#### 模块信息

```
GET /info
```

#### 语音合成

```
POST /predict/synthesize
Content-Type: application/json

{
  "input_text": "你好世界",
  "params": {
    "voice": "default",
    "speed": 1.0,
    "sample_rate": 24000
  }
}
```

响应：

```json
{
  "status": "ok",
  "output_path": "/path/to/output.wav",
  "sample_rate": 24000,
  "duration_secs": 1.23,
  "engine": "qwen3-tts"
}
```

## 模型

| ID | 名称 | 大小 | 说明 |
|----|------|------|------|
| `1.7b` | Qwen3-TTS 1.7B | ~3.5 GB | 高质量，默认 |
| `0.6b` | Qwen3-TTS 0.6B | ~1.3 GB | 轻量快速 |

模型从 [ModelScope](https://modelscope.cn) 下载。

## 许可

Apache-2.0
