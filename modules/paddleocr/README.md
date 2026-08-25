# PaddleOCR 文字识别模块

基于 [PaddlePaddle/PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) 的多语言 OCR 模块，支持文字检测、识别和方向分类，并提供 **PP-StructureV3 文档理解**（Doc→Markdown）能力。

## 功能

- **文字检测** — 定位图片中的文字区域（四点坐标）
- **文字识别** — 将检测到的区域转为文本
- **方向分类** — 自动纠正旋转/倒置文字
- **多语言** — 中文、英文、日文、韩文等 80+ 语种
- **文档理解（doc_understand）** — PP-StructureV3 全链路：版面分析、表格结构（含 HTML）、公式 LaTeX、图表转表格、文档方向分类、文档去扭曲；输入图片或 PDF（dpi 150–600 逐页渲染）

## 接口

| 端点 | 方法 | 说明 |
|------|------|------|
| `/health` | GET | 健康检查 |
| `/info` | GET | 模块信息与参数说明 |
| `/predict/recognize` | POST | 图片文字识别 |
| `/predict/doc_understand` | POST | 文档理解（图片/PDF → Markdown） |

### POST /predict/recognize

支持两种输入方式：

1. **JSON body** — `{"input_path": "/path/to/image.png"}`
2. **Multipart form** — 上传 `file` 字段

可选参数（query 或 JSON body）：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `lang` | string | `ch` | 语言代码 |
| `use_angle_cls` | boolean | `true` | 启用方向分类 |
| `det_db_thresh` | float | `0.3` | 检测阈值 (0.0–1.0) |

### 响应示例

```json
{
  "status": "completed",
  "output_type": "json",
  "result": {
    "text": "完整识别文本",
    "lines": [
      {
        "text": "第一行",
        "confidence": 0.98,
        "bbox": [[x1, y1], [x2, y2], [x3, y3], [x4, y4]]
      }
    ],
    "language": "ch"
  }
}
```

### POST /predict/doc_understand

输入与 recognize 同契约（multipart `file` 或 JSON `input_path`），支持：
- **图片**（PNG/JPG/BMP/WEBP/TIF）— 单文档
- **PDF** — 按 `dpi`（150–600，默认 300）逐页渲染为 PNG 后逐页解析（pdf2image 优先，pypdfium2 回退；上限 100 页）

参数（`params` JSON）：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `doc_orientation` | boolean | `false` | 文档方向分类（慢） |
| `doc_unwarping` | boolean | `false` | 文档去扭曲（慢） |
| `table` | boolean | `true` | 表格结构识别 |
| `formula` | boolean | `true` | 公式识别（LaTeX） |
| `chart` | boolean | `true` | 图表识别（转入表格） |
| `dpi` | integer | `300` | PDF→图片 DPI（150–600） |
| `output_format` | select | `json` | `json`=markdown+页面结构；`md`=纯 Markdown |

响应：`output_format="json"` 时 `result` 为 `{markdown, pages:[{index, markdown, structure}], input:{kind,pages,dpi}, options}`；`output_format="md"` 时直接返回 Markdown 字符串（`output_type="text"`，执行器注入 `output_path` 时写为该文件并返回 `output_type="file"`）。

**模型要求**：变体 `pp-structure-v3`（target_dir = `paddleocr-pp-structure-v3`）须经「模型管理 → 本地导入」落位，内部含 14 个子模型目录（`layout/ region/ doc_ori/ doc_unwarping/ ocr_det/ ocr_rec/ formula/ chart/ table_cls/ table_wired_structure/ table_wired_cells/ table_wireless_structure/ table_wireless_cells/ textline_orientation/`，chart 内部为 HF 风格 `PP-Chart2Table/`）。adapter 先探测目录再装载，不硬编码文件名；缺失时返回 503 `MODEL_NOT_LOADED`，**不会**静默联网下载。

## 计算后端

- **CUDA** (默认) — 需要 NVIDIA GPU，VRAM ≥ 512 MB
- **CPU** — 无需 GPU，速度较慢（PP-StructureV3 CPU 上仅做离线推理，禁止联网下载）

## 模型

| ID | 名称 | 大小 |
|----|------|------|
| `v4-chinese` | PP-OCRv4 中文 (推荐) | ~200 MB |
| `v4-multilingual` | PP-OCRv4 多语言 | ~200 MB |
| `pp-structure-v3` | PP-StructureV3 文档理解（本地导入） | ~3.1 GB |

## 越权需求（INT / W3 处理）

1. **环境依赖**：`PPStructureV3` 需要 paddleocr/paddlex 3.3+ 与 paddlepaddle 3.x（模块 venv 可能缺 paddlex）——可由源 zip `python312/Lib/site-packages` 拷贝或 pip 安装，**不改现有 requirements.txt / runtime venv 结构语义**（决定权在 INT）。
2. **PDF→图片依赖**：pdf2image>=1.17（poppler/pypdfium2 后端）或 pypdfium2（源 zip 环境内已随 paddlex 自带）二者任一即可。
3. **权重导入**：`PP-StructureV3-gpu-offline.zip` 中 `models/`（14 子模型 ≈3.1GB，含 chart/PP-Chart2Table/model_state.pdparams 1.4GB）拷贝入 `<models_cache>/paddleocr-pp-structure-v3/`（W2）。
