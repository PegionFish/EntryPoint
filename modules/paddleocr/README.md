# PaddleOCR 文字识别模块

基于 [PaddlePaddle/PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) 的多语言 OCR 模块，支持文字检测、识别和方向分类。

## 功能

- **文字检测** — 定位图片中的文字区域（四点坐标）
- **文字识别** — 将检测到的区域转为文本
- **方向分类** — 自动纠正旋转/倒置文字
- **多语言** — 中文、英文、日文、韩文等 80+ 语种

## 接口

| 端点 | 方法 | 说明 |
|------|------|------|
| `/health` | GET | 健康检查 |
| `/info` | GET | 模块信息与参数说明 |
| `/predict/recognize` | POST | 图片文字识别 |

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

## 计算后端

- **CUDA** (默认) — 需要 NVIDIA GPU，VRAM ≥ 512 MB
- **CPU** — 无需 GPU，速度较慢

## 模型

| ID | 名称 | 大小 |
|----|------|------|
| `v4-chinese` | PP-OCRv4 中文 (推荐) | ~200 MB |
| `v4-multilingual` | PP-OCRv4 多语言 | ~200 MB |
