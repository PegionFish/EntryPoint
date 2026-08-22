# video-upscale — 视频超分模块（W1 脚手架，实验性）

Real-ESRGAN 官方系公版权重（BSD-3-Clause）的视频超分辨率模块。
三运行时分层（HETERO_DIST_PLAN §3.1，厂商栈优先，ncnn-vulkan 仅兜底）：

| backend | 运行时 | 状态 |
|---|---|---|
| cuda / rocm / cpu | torch + `.pth`（懒导入） | 结构完整；依赖见 requirements-torch.txt（M2 落地前手动装）；未装时返回 501 EXPERIMENTAL |
| openvino | onnxruntime OpenVINO EP（ORT-OV） | 分支就绪；**ONNX 变体占位待补**（module.toml 注释块 + reports/ws-f-engine-choice.md §1.2），权重落位前恒 501 EXPERIMENTAL |
| vulkan | nihui/xinntao ncnn 引擎子进程 | subprocess 结构完整；引擎二进制需按下述步骤放置 |

## 引擎二进制放置（vulkan 兜底）

二进制不随模块包分发，从上游官方 release 获取（勿用 Waifu2x-Extension-GUI 的
`_W2xEX` 重编译版）：

```bash
# Linux（NAS）
bash scripts/fetch-engine.sh linux
# 或手动：
#   https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesrgan-ncnn-vulkan-20220424-ubuntu.zip
#   解压出 realesrgan-ncnn-vulkan 放到 bin/linux-x86_64/
```

```powershell
# Windows
bash scripts/fetch-engine.sh windows
# 或手动取 -windows.zip 同 tag 资产，解压出 realesrgan-ncnn-vulkan.exe 放到 bin/windows-x86_64/
```

## 模型变体

| id | 格式 | 用途 | 来源 |
|---|---|---|---|
| realesr-animevideov3-pth | .pth | 视频轻量默认（torch 路线） | xinntao release v0.2.5.0 |
| realesrgan-x4plus-pth | .pth | 照片级通用 | xinntao release v0.1.0 |
| animevideo-xsx2/-xsx4-pth | .pth | 动漫视频 v2 系列 | xinntao release v0.2.3.0 |
| realesrgan-animevideov3-x4-ncnn | zip 整包 | ncnn/vulkan 兜底 | v0.2.5.0 portable 包（含模型+引擎） |
| *realesrgan-x4plus-onnx* | .onnx | OpenVINO（占位待补） | 自转换后回填 |

注意：平台 URL 下载仅自动解压 tar.gz/tgz；ncnn 变体的上游资产是 **zip**，
下载后请手动解压（或走 WebUI 浏览器上传导入，上传路径支持 zip 自动解包）。

## 手动验证

```bash
export EP_MODULE_DIR=$PWD EP_PORT=8920 EP_BACKEND=vulkan \
       EP_MODEL_DIR=/path/to/models/video-upscale-realesr-animevideov3-x4-ncnn \
       EP_WORKSPACE=/tmp/ep-ws
python adapter.py &
curl http://127.0.0.1:8920/health
curl http://127.0.0.1:8920/info
curl -X POST http://127.0.0.1:8920/predict/upscale -H 'Content-Type: application/json' \
     -d '{"input_path":"/path/to/in.mp4","params":{"scale_factor":4,"target_preset":"balanced"}}'
```
