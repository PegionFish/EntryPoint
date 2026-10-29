# OpenAI 兼容桥接（openai-bridge）

把任意 OpenAI 兼容 API 接入 EntryPoint 能力目录，与本地 GPU 模块**同权混用**：

- **云端**：OpenRouter / SiliconFlow / DeepSeek / Groq / OpenAI …
- **本地自启实例**：vLLM / Ollama / LM Studio …

桥接模式（策略 C）：模块不托管上游进程，只外连；`type="python"` + `backends=["cpu"]`
（代理不做计算）。先启动你的上游服务/准备好密钥，再跑管线。

## 三能力

| 能力 | 上游端点 | 典型用途 |
|---|---|---|
| `chat` | `/chat/completions` | 字幕线翻译/润色/脚本节点（**SRT 入 → SRT 出**，时间轴原地保真） |
| `transcribe` | `/audio/transcriptions` | 云端 ASR 兜底（纯音频）；SRT 输入 = 云端字幕修正/翻译 |
| `speak` | `/audio/speech` | 云端 TTS 兜底/对比（配音线） |

### SRT 就地重组（字幕管线防漂移核心）

`chat` / `transcribe` 输入命中 SRT 时：解析时间轴 → 每条正文（剥离 `{\an8}` 等位置标签
与 `<i>` 等整条包裹标签）经 `##N##` 协议批量送上游 → 逐条回填。**输出 SRT 的每条
起止时间与序号与输入完全一致**；模型丢行自动逐条补调，映射不齐明确报错（绝不静默错位）。

### context_in（可选输入端口）

参考文档/术语表/VI 规范/前文 → 注入 system 上下文。接受 JSON body `context_in`
（字符串或数组）、multipart `context_in` 字段、文件名以 `context` 开头的文件部件、
`params.context_in`。

## 密钥注入（零明文落盘）

模块目录与导出内**永不**出现密钥。三选一（优先级从高到低）：

1. `params.api_key` —— 运行时值（调用方/编排侧注入）；
2. `params.key_env` —— 环境变量**名**引用（key_ref 形态，如 `"key_env": "SILICONFLOW_KEY"`）；
3. 环境变量 `OPENAI_API_KEY`。

服务端集中管理（提供商 profile / 掩码 / 测试连接）由平台「LLM 提供商 profile」承担。

## 参数速查

公共：`model`（必填）、`base_url`（须含版本段如 `…/v1`；覆盖 env `OPENAI_BASE_URL`）、
`api_key` / `key_env`、`timeout`（秒，缺省 120）。

- `chat`：`system_prompt`、`context_in`、`temperature`（0–2）、`response_format`（透传）、
  `output_format`（`json`/`txt`/`srt` 文件产物模式）
- `transcribe`：`language`、`prompt`（领域词引导）、`response_format`（json/verbose_json/srt/vtt/text）、
  `output_format`（`srt` = 字幕文件产物）
- `speak`：`voice`（缺省 `alloy`）、`response_format`（mp3/opus/aac/flac/wav/pcm）、`speed`

每次调用产 `usage.json` 侧产物（任务工作目录优先）：prompt/completion/total tokens
逐次与合计（上游不返回时如实记 null）。

## 快速开始

```bash
# 1) 起 mock 上游自测（或指向你的 vLLM/Ollama/云供应商）
python tests/mock_openai.py --port 8199

# 2) 起模块
EP_PORT=8186 OPENAI_BASE_URL=http://127.0.0.1:8199/v1 python adapter.py

# 3) 调用
curl -s http://127.0.0.1:8186/health
curl -s -X POST http://127.0.0.1:8186/predict/chat \
  -H "Content-Type: application/json" \
  -d '{"input":"hello","params":{"model":"mock-chat","api_key":"sk-x"}}'
```

## 测试

```bash
python -m pytest tests/ -q   # 全程 mock 上游，不访问外网
```

## 已知边界

- SRT 重组仅支持毫秒逗号时间戳（VTT/ASS 不重组）；正文行内标签保留为 best-effort；
- `embed` / `generate_image` / n>1 多候选目录产物 / 任务元数据 usage 记账 → M2；
- 契约细节与错误映射见 [CONTRACT.md](CONTRACT.md)。
