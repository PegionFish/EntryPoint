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

可选参数（参数面与源应用 libDF CLI `enhance_wav` 对齐）：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `attenuation` | int | 100 | 降噪强度 (dB)，0–100 |
| `min_db` | float | -60.0 | 最小增益 (dB)，-100–0 |
| `post_filter` | bool | false | 启用后置滤波（高噪段补偿） |
| `pf_beta` | float | 0.02 | 后置滤波强度，0–2 |
| `min_db_thresh` | float | -15.0 | DNN 本地 SNR 阈值下限，-100–0 |
| `max_db_erb_thresh` | float | 35.0 | ERB 解码器 SNR 上限，0–100 |
| `max_db_df_thresh` | float | 35.0 | DF 解码器 SNR 上限，0–100 |
| `reduce_mask` | int | 1 | 多声道 mask 归并：1=max，2=mean |
| `compensate_delay` | bool | false | 补偿 STFT/模型 lookahead 延迟 |

> 接线说明（libDF CLI → deepfilternet python API 映射）：
>
> | CLI 参数 | 传递方式 |
> |----------|----------|
> | `--atten-lim-db` | `enhance(atten_lim_db=...)` |
> | `-D/--compensate-delay` | `enhance(pad=...)`（默认语义见下） |
> | `--pf/--pf-beta` | `enhance(post_filter/pf_beta=...)`（deepfilternet ≥0.6）或运行时切换模型属性（0.5.6，仅 DeepFilterNet3 支持） |
> | `--min-db-thresh` / `--max-db-erb-thresh` / `--max-db-df-thresh` / `--reduce-mask` | `enhance()` 同名 kwargs（deepfilternet ≥0.6）；0.5.6 python 后端无此运行时旋钮 → 警告后忽略（行为不降级，仍为库默认值） |
> - deepfilternet 0.5.6 的 `enhance()` 不接受 `min_db` 参数，该参数由 adapter 在 enhance 后以频谱增益下限（幅度钳制 `floor × |原信号|`）实现，API 语义不变。
> - 新参数严格校验：乱参/越界 → 400 `INVALID_PARAM`；既有 `attenuation`/`min_db` 仍为钳制解析。
> - `compensate_delay` 未显式传参时保持现有行为（延迟补偿开启，输出与输入等长）；显式 `false` 才关闭补偿。module.toml 名义默认 `false` 表示「默认不要求补偿」。

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
