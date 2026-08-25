# Qwen3-TTS 语音合成模块

基于 [Qwen3-TTS](https://github.com/QwenLM/Qwen3-TTS)（官方 `qwen-tts` SDK）的高质量多语言语音合成模块。
功能面对齐源应用 [AI_Applications/Qwen3TTS](../../../AI_Applications/Qwen3TTS) 的 WebUI（MODULE_PARITY_PLAN §3 A1）。

## 能力（capabilities）

| 能力 | 模型 / 变体 | 说明 |
|------|-------------|------|
| `synthesize` | VoiceDesign（或 CustomVoice 兼容） | 文本转语音；`instruct` 自然语言声音描述；`language` 11 项枚举（auto + 10 正名） |
| `clone_voice` | Base | 参考音频声音克隆；`ref_text` 留空 → **x-vector 零样本克隆**，填写 → ICL 提示克隆 |
| `custom_voice` | CustomVoice | 内置 9 音色 + 情绪/风格 instruct（如"生气""开心"） |

- 三个能力各自使用 source (AI_Applications) 的对应 1.7B 模型（VoiceDesign / Base / CustomVoice），由激活变体（EP_MODEL_ID）流转；能力所需模型类型与激活变体不一致时，adapter 在 `EP_MODELS_ROOT` 下按 manifest target_dir 自动换载对应变体。
- 缺本地权重时**不联网下载**，返回 503 并提示导入路径；clone 失败（模型不可用）时回退 `synthesize` 并在 `metadata.note` 标注。

## 快速开始

### 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `EP_MODULE_DIR` | 模块根目录 | 当前目录 |
| `EP_MODEL_DIR` | 激活变体模型目录（daemon 注入 `<models_cache>/<target_dir>`） | `EP_MODULE_DIR/models` |
| `EP_MODELS_ROOT` | 模型缓存根（非激活变体定位，经 manifest target_dir） | 无（回退模型目录父级） |
| `EP_WORKSPACE` | 输出工作目录 | 当前目录 |
| `EP_PORT` | HTTP 服务端口 | `8000` |
| `EP_DEVICE` / `EP_BACKEND` | 计算后端（`cuda:0` / `cpu`） | `cuda` |
| `EP_MODEL_ID` | 激活变体 ID（`1.7b` / `0.6b` / `tts-voice-design` / `tts-base-clone` / `tts-custom-voice`） | `1.7b` |

### 启动服务

```bash
pip install -r requirements.txt
python adapter.py
```

模块自检（不加载模型、纯标准库）：

```bash
python adapter.py --selftest
```

### API

#### 语音合成（VoiceDesign / CustomVoice）

```
POST /predict/synthesize
{
  "input_text": "你好世界",
  "params": { "language": "chinese", "instruct": "甜美萝莉音" }
}
```

#### 声音克隆（Base；ref_text 留空为 x-vector 零样本）

```
POST /predict/clone_voice
{
  "input_text": "今天天气不错",
  "params": { "ref_audio": "<上传后回填的服务器路径>.wav", "ref_text": "" }
}
```

#### 内置音色（CustomVoice）

```
POST /predict/custom_voice
{
  "input_text": "你好世界",
  "params": { "spk_id": "Vivian", "instruct": "生气" }
}
```

响应统一契约（success）：

```json
{
  "status": "completed",
  "output_type": "file",
  "result": "/workspace/tts_...wav",
  "output_path": "/workspace/tts_...wav",
  "metadata": {"engine": "qwen3-tts", "capability": "custom_voice", "sample_rate": 24000,
               "duration_secs": 1.23, "language": "Chinese", "model_type": "custom_voice",
               "speaker": "Vivian"},
  "elapsed_seconds": 1.23
}
```

## 模型变体

| ID | 名称 | target_dir | 大小 | 说明 |
|----|------|------------|------|------|
| `1.7b` | Qwen3-TTS 1.7B (高质量) | `qwen3-tts-1.7b` | ~4.5 GB | CustomVoice（默认，兼容旧） |
| `0.6b` | Qwen3-TTS 0.6B (轻量) | `qwen3-tts-0.6b` | ~2.5 GB | CustomVoice（无 instruct 风格） |
| `tts-voice-design` | Qwen3-TTS 1.7B VoiceDesign | `qwen3-tts-12hz-1.7b-voice-design` | ~4.5 GB | 声音设计（instruct） |
| `tts-base-clone` | Qwen3-TTS 1.7B Base(克隆) | `qwen3-tts-12hz-1.7b-base-clone` | ~4.5 GB | 参考音频克隆 |
| `tts-custom-voice` | Qwen3-TTS 1.7B CustomVoice | `qwen3-tts-12hz-1.7b-custom-voice` | ~4.5 GB | 内置音色 |

三个 1.7B 变体权重可从本机 `/home/bob/AI_Applications/Qwen3TTS/models/Qwen3-TTS-12Hz-1.7B-*` 本地导入（模型管理器 → 本地导入，指定 target_dir），支持 CUDA bf16（约 8GB 显存）与 CPU fp32 兜底。

## 语言枚举

`auto` + `chinese` / `english` / `german` / `italian` / `portuguese` / `spanish` / `japanese` / `korean` / `french` / `russian`。
北京/四川方言随特定音色（Dylan/Eric）自动切换，不在枚举内。

## 许可

Apache-2.0
