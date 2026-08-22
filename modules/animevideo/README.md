# animevideo — 动漫视频超分模块（家族拆分，实验性）

RealESRGANv2-animevideo 系公版权重（BSD-3-Clause）的视频超分模块。
自 video-upscale 按模型家族拆分而来（平台约定：**一个模块 = 一个模型家族**，
编排按家族而非功能类别组织）。

| backend | 运行时 | 状态 |
|---|---|---|
| cuda / rocm / cpu | torch + `.pth`（懒导入） | 结构完整；basicsr/realesrgan 经 `scripts/post-install.sh` 两步安装（tb-nightly 规避），未装时返回 501 EXPERIMENTAL |

说明：官方无 animevideo 系 ncnn/ONNX 权重发布（决策备忘录
reports/ws-f-engine-choice.md §1.2），故不声明 vulkan/openvino 后端。

## 权重变体

| id | 说明 | 来源 |
|---|---|---|
| animevideo-xsx2-pth | 动漫视频 2x（默认） | xinntao/Real-ESRGAN v0.2.3.0 release |
| animevideo-xsx4-pth | 动漫视频 4x | 同上 |

架构映射（adapter `_srvgg_preset`，实证键集+形状全匹配）：
xsx2 → SRVGG(num_conv=16, upscale=2)；xsx4 → SRVGG(32, 4)。
请求的 scale_factor 经 `enhance(outscale=…)` 落实，与基网倍率解耦。

## 与 realesr 模块的关系

同源仓库（xinntao/Real-ESRGAN）、共享 adapter 实现与依赖链；
差异仅在权重变体与后端声明。家族拆分是为了编排语义清晰：
realesr = 主线轻量/通用系，animevideo = v2 动漫视频专用系。
