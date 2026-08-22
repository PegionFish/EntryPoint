# Qwen3-ASR（qwen3-asr）

EntryPoint ASR 模块：[Qwen3-ASR](https://github.com/QwenLM/Qwen3-ASR) 语音识别 +
[Qwen3-ForcedAligner](https://huggingface.co/Qwen/Qwen3-ForcedAligner-0.6B) 词级时间戳强制对齐。

- **category / genre**：`asr` / `qwen-asr`
- **能力**：`transcribe`（audio → json，支持 context 提示文本偏置与词级时间戳）、
  `align`（audio + 参考文本 → json 词级时间戳）
- **后端**：`cuda`（bf16，推荐）/ `cpu`（fp32 兜底）；rocm 本期不纳入（HETERO_DIST_PLAN D2）

## 来源与许可

| 项目 | 出处 | 许可 |
|---|---|---|
| 推理库 `qwen-asr` | <https://pypi.org/project/qwen-asr/>（官方 GitHub 同仓） | Apache-2.0 |
| Qwen3-ASR-0.6B / -1.7B | HF `Qwen/Qwen3-ASR-0.6B`、`Qwen/Qwen3-ASR-1.7B`（ModelScope 同名镜像） | Apache-2.0 |
| Qwen3-ForcedAligner-0.6B | HF `Qwen/Qwen3-ForcedAligner-0.6B`（ModelScope 同名镜像） | Apache-2.0 |

三张权重卡片均标注 `License: apache-2.0`、无 gated 门禁，代码与权重同许可 →
**Tier A 可捆绑再分发**（证据卡见 `reports/license-matrix.md` §9）。模块包不携带权重，
经平台模型管理器下载（HF 主源 + ModelScope 镜像双源可选）。

## 硬件建议

| 变体 | 权重体积 | 建议显存（bf16/cuda） | CPU 回退 |
|---|---|---|---|
| qwen3-asr-0.6b（默认） | ~1.8 GB | ≥ 2.5 GB VRAM | fp32，可用但慢 |
| qwen3-asr-1.7b | ~4.4 GB | ≥ 5.5 GB VRAM | fp32，仅短音频 |
| qwen3-forced-aligner-0.6b（随 ASR 挂载） | ~1.8 GB | 与上表叠加约 +1 GB 运行开销 | — |

说明：
- CUDA 路径使用 bfloat16（Blackwell sm_120 原生支持），torch 锁 2.11.0 + cu130 wheel 索引。
- Aligner 权重存在时随 ASR 一并挂载（官方 demo 形态），`transcribe(timestamps=true)`
  与 `align` 复用同一实例，不重复占显存；缺失时 transcribe 自动降级为纯文本
  （响应中 `timestamps_degraded=true`），align 返回 503 并给出获取指引。

## 与 faster-whisper（genre=whisper）的对比

同属 `asr` 类别但 genre 不同，管线中按需选型：

| 维度 | qwen3-asr（本模块，genre=qwen-asr） | faster-whisper（genre=whisper） |
|---|---|---|
| 架构 | LLM 式音频条件生成（Transformers/torch） | Whisper 编解码器（CTranslate2 int8/fp16） |
| 上下文提示 | **context 参数原生支持**：热词/专名/领域描述直接注入提示词，显著改善专名召回 | 无一等价机制（initial_prompt 效果有限） |
| 时间戳 | 词级时间戳由独立的 ForcedAligner 模型产出（更精细，CJK 按字对齐） | 内建 word_timestamps（交叉注意力启发式） |
| 语种 | 30 种（中文/多方言场景强项），语言名或 auto | ~99 种 VTT 语种 |
| 部署形态 | torch 全家桶，显存占用较高 | CT2 量化，CPU 友好、显存低 |
| 选型建议 | 中文为主、需要热词偏置、需要高精度字幕对齐 | 多语种混合、低资源机器、长音频批量 |

## REST 接口

```bash
# 健康检查 / 信息
curl http://127.0.0.1:18002/health
curl http://127.0.0.1:18002/info

# 转写（multipart）
curl -X POST http://127.0.0.1:18002/predict/transcribe \
  -F "file=@test.wav" \
  -F 'params={"language": "zh", "context": "这是一段关于量子计算的访谈", "timestamps": true}'

# 转写（JSON 路径输入，长音频分段在库内自动处理）
curl -X POST http://127.0.0.1:18002/predict/transcribe \
  -H "Content-Type: application/json" \
  -d '{"input_path": "/abs/path/test.wav", "params": {"language": "auto"}}'

# 强制对齐（参考文本必填，language 必填——align 不做自动检测）
curl -X POST http://127.0.0.1:18002/predict/align \
  -H "Content-Type: application/json" \
  -d '{"input_path": "/abs/path/test.wav",
       "input_text": "你好世界，这是一个测试。",
       "params": {"language": "zh"}}'
```

响应遵循 ADAPTER_API.md §2.3 信封；transcribe 的 `result.segments` 为句级
`[{start,end,text}]`（词级时间戳按标点断句聚合，口径与官方 demo 一致），
`result.words` / align 的 `result.words` 为词级 `[{word,start,end}]`（秒，
CJK 按字、拉丁按词，字段直接映射上游 ForcedAlignItem，未额外造字段）。

`language` 参数接受规范语言名（`Chinese`…30 种）或常用 ISO 代码（`zh/en/ja/yue/...`）；
`params.model` 支持变体临时覆盖（`0.6b` / `1.7b`），从 `EP_MODELS_ROOT` 解析本地权重，
缺失时报 503 且不联网下载（ADAPTER_API.md §1.3 契约）。

## 已知限制

- `align` 不支持语种自动检测，必须显式传 language；建议单次对齐音频 ≤30 秒
  （超长音频请先用 transcribe 的内建分段，或自行切片后逐段 align）。
- Aligner 需与 ASR 同时驻留内存（挂载形态）；纯 CPU 机器建议关闭
  `timestamps` 以跳过对齐开销。
- 首次请求触发懒加载（数秒至数十秒），期间该请求阻塞直至加载完成；
  `/health` 在进程就绪后即返回 ok。
