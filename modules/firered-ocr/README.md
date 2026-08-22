# FireRed-OCR 文档识别模块

基于 [FireRed-OCR](https://github.com/FireRedTeam/FireRed-OCR)（小红书 FireRedTeam）的端到端文档解析 OCR：将文档图片转为结构化 Markdown（标题/段落/列表/LaTeX 公式/HTML 表格）。

## 来源与许可

| 项 | 值 |
|---|---|
| 模型仓库 | `FireRedTeam/FireRed-OCR`（HuggingFace，官方；ModelScope 同名仓库为官方镜像，已登记为 mirror） |
| 基座 | Qwen3-VL-2B-Instruct 微调 |
| 参数规模 | 2,127,532,032（BF16 safetensors ≈ **2.1B**，权重合计 ≈ 4058 MiB）——**非 8B 级** |
| 许可 | **Apache-2.0**（代码与权重均然；HF `LICENSE.txt` 与 GitHub Apache License 2.0 已核实） |
| 推理路径 | 官方 `main.py` / Quick Start：`transformers.Qwen3VLForConditionalGeneration` + `AutoProcessor`，chat template + 贪心生成为 Markdown |

## 功能

- **recognize（image → json）**：文档图片 → Markdown 文本
- 可选 `languages` 语言提示（逗号分隔或 JSON 列表，如 `"zh,en"`）
- `output_format = text/md` + 执行器注入的 `output_path` 时输出文件产物（MODULE_SPEC §5）
- 后端：CUDA（BF16）/ CPU（FP32）

## 硬件建议

| 设备 | 说明 |
|---|---|
| GPU ≥ 6 GB 显存 | 推荐。2B BF16 权重 ~4.1 GB + 视觉塔激活/KV cache/驱动上下文 ≈ 6 GB |
| GPU 4–6 GB | 勉强可跑长图可能吃紧（`min_vram_mb = 4096` 会告警） |
| CPU | 可运行（FP32），单页数十秒级，仅建议轻量验证 |

## API

```bash
# 路径输入（管线内部优先）
curl -X POST http://127.0.0.1:18001/predict/recognize \
  -H "Content-Type: application/json" \
  -d '{"input_path": "/path/to/page.png", "params": {"languages": "zh,en"}}'

# 文件上传
curl -X POST http://127.0.0.1:18001/predict/recognize \
  -F "file=@page.png" \
  -F 'params={"max_new_tokens": 8192}'
```

成功响应（模型只产出 Markdown 文本，无坐标框/置信度，故 result 仅含 `text`）：

```json
{
  "status": "completed",
  "output_type": "json",
  "result": {"text": "# 标题\n\n正文……"},
  "output_path": null,
  "elapsed_seconds": 12.3
}
```

## 参数

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `languages` | string/list | 空 | 可选语言提示，逗号分隔（适配器亦接受 JSON 列表）；留空不注入提示行 |
| `max_new_tokens` | integer | 8192 | 生成上限（官方默认 8192） |
| `output_format` | select | json | `text`/`md` 配合 `output_path` 输出文件产物 |

## 依赖说明

requirements 以官方 `main.py` 的推理期依赖为准：`transformers>=4.57.0`（Qwen3-VL 架构自 4.57 起）、`torch==2.11.0`（cu130 索引，对齐 constraints.txt 全家桶锁）、`accelerate`（官方 `device_map="auto"` 所需）、`Pillow`。官方 main.py 另含 `gradio`（WebUI）与 `pdf2image`（PDF 转图，需系统 Poppler）——本适配器仅封装图片推理路径，二者未纳入；PDF 场景请在管线中先行转图。

## 已知限制

- 无检测框/置信度输出（与上游能力一致；需要 bbox 请用 paddleocr 模块）
- 单图逐页处理；多页 PDF 需管线拆页
- 模型目录缺失时返回 503 `MODEL_NOT_LOADED`，不静默联网下载（ADAPTER_API §1.3）
