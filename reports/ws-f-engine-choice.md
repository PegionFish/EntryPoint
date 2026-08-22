# WS-F 决策备忘录：video-upscale / video-interp 引擎与运行时选型

> 波次：W1 | 代理：WS-F | 日期：2026-08-22
>
> 依据：docs/HETERO_DIST_PLAN.md §3.1/§3.2/§5(E6-E8)、docs/MODULE_SPEC.md §2/§5、
> 计划 §9 已定案「SR/VFI 运行时 = 厂商栈优先（CUDA/ROCm=torch、Intel=OV-ONNX），
> ncnn-vulkan 仅兜底」。本文所有直链均为当日实测可达（GitHub API 核对资产清单）。

---

## 1. 三条运行时路线对比

### 1.1 torch 路线（CUDA / ROCm 直跑，主路线）

| 维度 | video-upscale | video-interp |
|---|---|---|
| 权重格式 | `.pth`（xinntao 官方 release） | `flownet.pkl`（Practical-RIFE train_log） |
| 许可 | BSD-3-Clause（官方系公版） | MIT（全系已核实，计划 §2.5） |
| 推理栈 | torch(+CUDA/ROCm wheel) + RRDBNet/compact arch（realesrgan pip 包或 vendor 网络定义） | torch + IFNet_HDv3（需 vendor Practical-RIFE 的 `rife_ifnet.py`/`rife_warplayer.py` 推理模块） |
| ROCm 可行性 | torch 官方 ROCm wheel（gfx1100 在支持矩阵内），代码零改动（`device="cuda"` 即 HIP） | 同左；E6 载体 |

**权重直链（已核实）：**

| 变体 | URL | 大小 |
|---|---|---|
| RealESRGAN_x4plus.pth | `https://github.com/xinntao/Real-ESRGAN/releases/download/v0.1.0/RealESRGAN_x4plus.pth` | 63.9 MB |
| realesr-animevideov3.pth | `https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesr-animevideov3.pth` | 2.4 MB |
| RealESRGANv2-animevideo-xsx2.pth | `https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.3.0/RealESRGANv2-animevideo-xsx2.pth` | 2.3 MB |
| RealESRGANv2-animevideo-xsx4.pth | `https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.3.0/RealESRGANv2-animevideo-xsx4.pth` | 2.4 MB |

RIFE torch 权重渠道限制（如实记录）：

- **官方渠道 = Google Drive + 百度网盘**（hzwer/Practical-RIFE README 表格）。GDrive 有
  每日配额与反爬直链问题（`FileURLRetrievalError`），**不适合作为平台自动下载源**；
- 社区镜像（MIT 同源核实）：`huggingface.co/Bash2X/RIFE-Models`（RIFE_v4.26.zip 等
  全系 zip 镜像）、`huggingface.co/gtomato/practical-rife-v4.25`（单文件 flownet.pkl，
  附 SHA256 与上游 zip 校验和溯源）。首版 manifest 采用 HF 镜像 url 直链，
  README 注明官方渠道与镜像性质；**镜像化建议**见 §5。

### 1.2 OpenVINO ONNX 路线（Intel iGPU/NPU）

| 维度 | 结论 |
|---|---|
| 上游是否发布 ONNX | **否。** xinntao 与 hzwer 均不随 release 发布官方 ONNX——两条路线的 ONNX 都需要转换或取社区镜像 |
| ESRGAN ONNX 获取途径 | ① 自行转换（推荐）：Intel 官方应用笔记《Using Real-ESRGAN on Intel Platform》(doc 816445) 给出 pth→ONNX(opset 12，静态/动态 shape)→OV IR 全流程，用上游自带 `scripts/pytorch2onnx.py`；② 社区镜像：`bukuroo/RealESRGAN-ONNX`（BSD-3，但输入固定 128×128，视频帧需切 128 tile，性能差）、Qualcomm AI Hub `qualcomm/Real-ESRGAN-x4plus`（同样固定 128 输入） |
| RIFE ONNX 获取途径 | 仅社区转换：`yuvraj108c/rife-onnx`（rife47/48/49 ensemble sim onnx）、`TensorStack/RIFE`、rigaya NVEnc 的 `--vpp-rife-ov` 配套模型仓（rigaya/HWEnc-onnx-models）。**无权威一手源、许可标注不全，暂不采纳为声明直链** |
| 运行方式 | ORT-OV：`onnxruntime-openvino` wheel + `providers=["OpenVINOExecutionProvider","CPUExecutionProvider"]`，设备经 `OPENVINO_DEVICE` 注入（E7=GPU.0，复用 rembg/onnx-matting 的 EP 选择思路） |
| 首版边界 | adapter OV 分支代码就位（找模型目录下 `*.onnx` → ORT-OV 会话）；**manifest 的 onnx 变体占位待补**——待自转出动 shape ONNX 并核验后再回填直链；在此之前 openvino 分支对缺失权重返回 501 EXPERIMENTAL |

### 1.3 ncnn-vulkan 兜底路线

| 维度 | video-upscale | video-interp |
|---|---|---|
| 引擎仓库 | **更正**：Real-ESRGAN ncnn 引擎上游是 `xinntao/Real-ESRGAN-ncnn-vulkan`（非 nihui；nihui 维护 rife/waifu2x/upscayl 系引擎），MIT，任务书所述"nihui 上游"按此落实为各引擎的真实官方仓 | `nihui/rife-ncnn-vulkan`，MIT |
| 二进制+模型包直链 | 模型捆绑版挂在主仓 Real-ESRGAN release v0.2.5.0：`realesrgan-ncnn-vulkan-20220424-windows.zip`(43.4MB) / `-ubuntu.zip`(44.8MB)，含 realesr-animevideov3 / x4plus / x4plus-anime / realesrnet-x4plus 的 param+bin；引擎仓自身 v0.2.0 的 zip 为纯二进制（无模型，2~8MB） | release 20221029：`rife-ncnn-vulkan-20221029-windows.zip` / `-ubuntu.zip`（各约 411MB，二进制 + 全部历代模型 rife-v4.6 等）；**无单独小体积模型包资产**（全部 release 页核对过） |
| 双平台可得性 | Windows/Linux 官方构建均有（macOS 亦有）；Linux 亦可自行构建 | 同左 |
| W2xEX 排除 | 不使用 Waifu2x-Extension-GUI 重编译版 exe（`_W2xEX` 后缀，计划 §3.2 ❌） | 同左 |
| 首版边界 | `[runtime.binaries]` 声明相对路径，用户从上游 release 解压放置 `bin/<os>-<arch>/`（README 给步骤）；adapter subprocess 结构完整；引擎缺失时返回明确错误而非静默失败 | 同左；注意 rife-ncnn 目录模式只输出中间帧，adapter 负责原帧/插值帧交错重组后按新帧率封装 |

### 1.4 cpu 后端

torch CPU 兜底（同 1.1 代码路径，`TORCH_DEVICE=cpu`），不做独立 ncnn-cpu 分支；
ncnn 引擎本身依赖 Vulkan 设备，CPU 无 Vulkan 时不可用。

---

## 2. 最终映射表（backend → 运行时 → 权重变体）

### video-upscale

| backend | 运行时 | 权重变体（[[models]] id） | 状态 |
|---|---|---|---|
| cuda | torch(.pth) | realesr-animevideov3-pth（默认）/ realesrgan-x4plus-pth / animevideo-xsx2-pth / animevideo-xsx4-pth | 结构完整，E4/E7 基线侧验证 |
| rocm | torch(.pth)（HIP） | 同上 | 结构完整，E6 旁路载体（interp 主实验在 interp 模块） |
| openvino | ORT-OV(.onnx) | *onnx 占位待补*（自转 dynamic-shape ONNX 后回填） | 501 EXPERIMENTAL（权重未定稿） |
| vulkan | ncnn 子进程 | realesrgan-animevideov3-x4-ncnn（20220424 包内 param+bin） | subprocess 结构完整，E8 载体 |
| cpu | torch(.pth) CPU | 同 cuda 变体 | 结构完整 |

### video-interp

| backend | 运行时 | 权重变体 | 状态 |
|---|---|---|---|
| cuda | torch(flownet.pkl) | rife-v4.26-pkl（HF 镜像 Bash2X/RIFE-Models） | 501 EXPERIMENTAL（需 vendor Practical-RIFE 推理模块） |
| rocm | torch(flownet.pkl)（HIP） | 同上 | **E6 主实验载体**，同上先 vendor |
| openvino | ORT-OV(.onnx) | 占位（社区 onnx 无权威源，暂不出声明） | 501 EXPERIMENTAL |
| vulkan | ncnn 子进程 | rife-v4.6-ncnn（nihui 20221029 包内 models/） | subprocess 结构完整，E8 载体 |
| cpu | torch(flownet.pkl) CPU | 同 cuda | 501 EXPERIMENTAL（同 vendor 项） |

> 未实现分支统一返回 `501 + error_code=EXPERIMENTAL_NOT_IMPLEMENTED`，message 注明
> 缺失物（ONNX 权重 / vendored 推理模块），符合任务书「未实现的分支返回 501
> EXPERIMENTAL 并注明」。

## 3. 视频管线骨架（两模块同构）

```
input.mp4 ──ffmpeg 抽帧(png 无损)──▶ frames_in/
            ├─ vulkan: ncnn 引擎子进程逐目录处理（upscale 直出同目录数；
            │          interp 只出中间帧 → adapter 交错重组）
            ├─ cuda/rocm/cpu: torch 懒导入分支
            └─ openvino: ORT-OV(OpenVINOExecutionProvider)
frames_out/ ──ffmpeg 封装(-c:v libx264 -crf，有音轨则 -c:a copy -map 0:a)──▶ output.mp4
```

产物遵循 MODULE_SPEC §5：响应 `output_type="file"` + `result`=输出绝对路径；
`params.output_path` 注入时优先写入该路径。

## 4. 首版落地边界（诚实声明）

1. torch 分支懒导入守卫完整：torch/realesrgan 缺失 → 501 EXPERIMENTAL（注明装哪个
   requirements 文件）；venv 内已具备时按 device 执行。
2. OV 分支：会话构建 + provider 选择完整；因 manifest onnx 变体未定稿，实际调用
   在权重落位前恒返回 501 EXPERIMENTAL。
3. ncnn 分支：引擎二进制存在性检查 → subprocess 调用结构完整（参数、超时、stderr
   回传），真机联调留 E8。
4. interp torch 路线阻塞于 vendor Practical-RIFE 推理 py（MIT，允许入库），W2 内补齐
   后 E6 才可执行。
5. 所有分支 experimental 标注已写入两 module.toml `[compute].notes`。

## 5. 镜像化建议（给编排者/WS-G）

1. **RIFE pkl**：建议由本组织建立一份带 SHA256 溯源的 HF 镜像仓（仿
   gtomato/practical-rife-v4.25 的 provenance 做法），替代第三方个人镜像作为
   manifest 主源；gdrive 只作 README 手动指引保留。
2. **ESRGAN ONNX**：按 Intel doc 816445 流程一次性自转 dynamic-shape ONNX
   （opset 12），校验 PSNR 对齐 torch 基线后挂自有 release，回填 manifest 占位。
3. **RIFE ncnn 大包**（411MB）建议在文档中引导"解压后仅保留所用模型目录"，避免
   NAS 用户整包留存；长期可向 nihui 提 issue 请求分模型资产。
4. 平台侧若支持 `[[models]]` 多文件条目（param+bin 两文件一个变体），可让 ncnn
   变体绕过"整包下载"；当前以整包 zip + 解压约定实现，见契约反馈。

---

## 附：核对过的上游资产清单（2026-08-22，GitHub API）

```
xinntao/Real-ESRGAN v0.1.0      : RealESRGAN_x4plus.pth (63.9MB)
xinntao/Real-ESRGAN v0.2.3.0    : RealESRGANv2-animevideo-xsx2/xsx4.pth (2.3/2.4MB)
                                  + realesrgan-ncnn-vulkan-20211212-{windows,ubuntu}.zip (~72MB)
xinntao/Real-ESRGAN v0.2.5.0    : realesr-animevideov3.pth (2.4MB), realesr-general(-wdn)-x4v3.pth,
                                  realesrgan-ncnn-vulkan-20220424-{windows,ubuntu,macos}.zip (43-49MB)
xinntao/Real-ESRGAN-ncnn-vulkan : v0.1.3.2 / v0.2.0 纯二进制 zip（windows/ubuntu/macos）
nihui/rife-ncnn-vulkan 20221029 : rife-ncnn-vulkan-20221029-{windows,ubuntu,macos}.zip (~411MB，
                                  含全部模型)；其余 9 个 release 同构，均无独立模型包资产
```
