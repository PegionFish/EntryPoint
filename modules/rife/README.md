# rife — 视频插帧模块（W1 脚手架，实验性）

RIFE 公版权重（**MIT 全系已核实**，HETERO_DIST_PLAN §2.5）视频插帧模块。
三运行时分层（厂商栈优先，ncnn-vulkan 仅兜底）：

| backend | 运行时 | 状态 |
|---|---|---|
| cuda / rocm / cpu | torch + `flownet.pkl`（懒导入） | **501 EXPERIMENTAL**：需 vendor Practical-RIFE 推理模块（`rife_ifnet.py`/`rife_warplayer.py`，MIT 可入库，W2 补齐）后方可运行，E6 实验载体 |
| openvino | onnxruntime OpenVINO EP（ORT-OV） | **501 EXPERIMENTAL**：社区 RIFE ONNX 无权威一手源（yuvraj108c/rife-onnx、TensorStack/RIFE 等许可标注不全），manifest 占位不出直链 |
| vulkan | nihui rife-ncnn-vulkan 子进程 | subprocess 结构完整；引擎二进制需按下述步骤放置 |

## 引擎二进制放置（vulkan 兜底）

```bash
# Linux（NAS）
bash scripts/fetch-engine.sh linux
# 或手动取：
#   https://github.com/nihui/rife-ncnn-vulkan/releases/download/20221029/rife-ncnn-vulkan-20221029-ubuntu.zip
```

```powershell
# Windows
bash scripts/fetch-engine.sh windows
# 或手动取 -windows.zip 同 tag 资产 → bin/windows-x86_64/rife-ncnn-vulkan.exe
```

勿用 Waifu2x-Extension-GUI 的 `_W2xEX` 重编译版引擎。

## 权重变体与获取渠道（如实记录）

| id | 格式 | 渠道 |
|---|---|---|
| rife-v4.6-ncnn（默认） | ncnn param+bin | nihui release 20221029 整包 zip（~411MB，含全部历代模型）。**上游无独立模型包资产**。zip 不在平台 URL 自动解压范围，下载后手动解压或浏览器上传导入；解压出的 `models/<rife-*/>` 目录保持原样 |
| rife-v4.26-pkl / v4.25-lite-pkl | flownet.pkl (torch) | **官方 = Google Drive/百度网盘**（hzwer/Practical-RIFE README 表格；GDrive 有每日配额与反爬，直链不稳定）。manifest 采用 HuggingFace 社区镜像 `Bash2X/RIFE-Models`（声明 MIT、未修改原文件）；组织自有镜像化建议见 `reports/ws-f-engine-choice.md §5` |

同一 ncnn 整包内可用 `params.model_name` 选择其他模型目录：
`rife-v4.6` / `rife-v4` / `rife-anime` / `rife-UHD` / `rife-HD` / `rife-v2.*`。

## 插帧管线说明

rife-ncnn-vulkan 目录模式只输出相邻两帧之间的中间帧（N 入 → N-1 出）；
adapter 负责：抽帧 → 引擎逐 pass 2x → 原帧/中间帧交错重组 → 非 2 的幂倍数等距
抽帧对齐 → ffmpeg 按 `源fps×倍数`（或 target_fps 推导倍数）回封 mp4（音轨 copy）。

## 手动验证

```bash
export EP_MODULE_DIR=$PWD EP_PORT=8921 EP_BACKEND=vulkan \
       EP_MODEL_DIR=/path/to/models/rife-rife-ncnn EP_WORKSPACE=/tmp/ep-ws
python adapter.py &
curl http://127.0.0.1:8921/health
curl -X POST http://127.0.0.1:8921/predict/interpolate -H 'Content-Type: application/json' \
     -d '{"input_path":"/path/to/in.mp4","params":{"multiplier":2}}'
```
