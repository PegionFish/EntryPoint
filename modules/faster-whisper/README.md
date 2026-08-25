# Faster-Whisper ASR Module

基于 [faster-whisper](https://github.com/SYSTRAN/faster-whisper) 的高速语音识别模块，使用 CTranslate2 推理引擎。

## 功能

- 多语言语音转文字（99 种语言）
- 词级时间戳
- VAD 静音过滤
- 支持 CUDA / ROCm / CPU 三种计算后端

## 支持的模型

| 模型 ID | 说明 | 大小 |
|---------|------|------|
| `large-v3` | 最高精度（默认） | ~3.1 GB |
| `large-v2` | 高精度 v2 | ~3.1 GB |
| `large-v1` | 高精度 v1 | ~3.1 GB |
| `medium` | 平衡精度与速度 | ~1.5 GB |
| `medium-en` | 英文专用版 medium | ~1.5 GB |
| `small` | 轻量快速 | ~500 MB |
| `small-en` | 英文专用版 small | ~500 MB |
| `base` | 快速 | ~145 MB |
| `base-en` | 英文专用版 base | ~145 MB |
| `tiny` | 最小 | ~75 MB |
| `tiny-en` | 英文专用版 tiny | ~75 MB |

英文专用模型（`-en` 后缀）对英文输入的识别速度与精度更优，其他语言请使用通用模型。

## 使用方法

### 启动

```bash
EP_PORT=18001 EP_MODEL_DIR=/path/to/model EP_DEVICE=cuda:0 python adapter.py
```

### 健康检查

```bash
curl http://localhost:18001/health
```

### 文件上传转写

```bash
curl -X POST http://localhost:18001/predict/transcribe \
  -F "file=@audio.wav" \
  -F 'params={"language":"zh","timestamps":true}'
```

### 路径转写

```bash
curl -X POST http://localhost:18001/predict/transcribe \
  -H "Content-Type: application/json" \
  -d '{"input_path": "/path/to/audio.wav", "params": {"language": "zh"}}'
```

## 参数

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `precision` | select | `"float16"` | 推理精度：`float16`/`int8`；CPU 自动锁定 `int8` |
| `language` | string | `"auto"` | 语言代码（如 `zh`、`en`），`auto` 自动检测 |
| `timestamps` | boolean | `true` | 输出词级时间戳 |
| `beam_size` | integer | `5` | Beam 搜索宽度（1-20） |
| `vad_filter` | boolean | `true` | 启用 VAD 静音过滤 |
| `condition_on_previous` | boolean | `true` | 上下文条件推理 |

## 依赖

- Python >= 3.10, < 3.13
- faster-whisper >= 1.0.0
- FastAPI + uvicorn
