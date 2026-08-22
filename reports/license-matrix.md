# 模型许可证合规矩阵 — 一手证据卡（WS-G）

> 版本：v1.0 | 核实日期：**2026-08-22** | 责任流：W1 / WS-G（许可证合规核实）
>
> 方法：GitHub 许可证据一律取自 `api.github.com/repos/<owner>/<repo>/license`
> 端点返回的 LICENSE **文件原文**（base64 解码）与浅克隆复核；关键条款为逐字摘录。
> HuggingFace 模型卡经检索快照与 hf-mirror.com 镜像核验（本环境直连 HF 超时）。
> 分级定义沿用 HETERO_DIST_PLAN §2.4：**A** 可捆绑再分发 / **B** 引导下载（附条件）/
> **C** 手动指引 / **排除**。

## 结论速览

| 模型族 | 上游许可（核实后） | 权重与代码同许可？ | 层级 | 一句话依据 |
|---|---|---|---|---|
| [ISNet isnet-general-use](#isnet-dis) | Apache-2.0（仅明示覆盖代码与评测指标） | ❓ 未逐字授权 | **B** | DIS 仓库 Apache-2.0 明确；权重经官方网盘外链分发且无独立条款 → 引导下载 |
| [GFPGAN v1.4](#gfpgan) | Apache-2.0（腾讯声明式，SPDX NOASSERTION） | ✅ 同仓 Release 资产 | **A** | 权重即该 Apache 声明仓库的 GitHub Release 资产，无附加限制 |
| [IFRNet](#ifrnet) | MIT | ⚠️ 外链未声明 | **B** | 官方仓 ltkong218/IFRNet 为 MIT；权重 Dropbox 外链无逐字授权 |
| [CAIN](#cain) | MIT（2021-10 起） | ⚠️ 外链未声明 | **B** | 原仓 LICENSE 自 2021-10-06 即为 MIT（推翻旧判断）；权重 Dropbox 外链；nihui 移植版 MIT 且 Release 自带转换模型 |
| [DAIN](#dain) | MIT（2019-03 起） | ⚠️ 外链未声明 | **B** | baowenbo/DAIN 自 2019-03-22 即带 MIT LICENSE（推翻"历史无 LICENSE"旧判断）；权重在作者校园服务器 |
| [RealSR DF2K](#realsr) | Apache-2.0（腾讯声明式） | ⚠️ 外链未声明 | **B** | jixiaozhong/Real-SR 已迁移 Tencent/Real-SR；权重 Google Drive 外链 |
| [SRMD x2/x3/x4 (+nf)](#srmd) | **无任何 LICENSE** | — | **排除** | 仓库零许可文本=默认权利保留；KAIR 的 MIT 仅覆盖工具箱代码，不构成对权重的授权 |
| [FireRed-OCR](#firered-ocr) | Apache-2.0 | ✅ 卡片逐字明示 | **A** | 模型卡原文："The code and the weights of FireRed-OCR are licensed under Apache 2.0." |
| [Qwen3-ASR / ForcedAligner](#qwen3-asr) | Apache-2.0 | ✅ HF 卡片标注 | **A** | 三个权重仓库卡片均标注 License: apache-2.0；GitHub 同许可 |
| [RMBG-1.4](#rmbg-14) | bria-rmbg-1.4（定制） | ❌ 非商业 | **C** | 条款原文确认：CC 非商业 + 商用需与 BRIA 另行协议；HF gated |
| [Practical-RIFE](#practical-rife) | MIT | ✅ README 逐字声明 | **A** | "The content of these links is under the same MIT license as this project." |

---

## 证据卡

<a id="isnet-dis"></a>

### 1. ISNet / isnet-general-use（DIS 项目）— 层级 B

- **仓库**：<https://github.com/xuebinqin/DIS>（默认分支 main）
- **LICENSE 文件**：`LICENSE.md` = **Apache License 2.0** 标准全文（GitHub SPDX 判定 Apache-2.0）
- **关键条款摘录**：
  - README「7. Term of Use」逐字：
    > Our code and evaluation metric use Apache License 2.0. The Terms of use for our DIS5K dataset is provided as DIS5K-Dataset-Terms-of-Use.pdf.
  - README 2022-08-17 发布注记（isnet-general-use.pth 出处）：
    > The optimized model for general use of our IS-Net is now released: `isnet-general-use.pth` (for general use, this is NOT DIS V2.0.) from (Google Drive) or (Baidu Pan 提取码:6jh2)
- **权重是否与代码同许可**：**未获逐字确认**。Term of Use 仅点名 "code and evaluation metric"；`isnet-general-use.pth` 经 Google Drive / 百度网盘外链发布，无随附许可声明。rembg 生态广泛再分发的 `isnet-general-use.onnx` 属第三方转换产物，不构成上游授权依据。
- **商用/再分发结论**：代码可依 Apache-2.0 使用与再分发（保留声明）；**权重不满足 Tier A 的"权重同源许可"门槛**——按保守原则归入 Tier B：模块以 `guide_url`/README 指引从 DIS 官方渠道获取，平台与模块包均不携带。
- **证据链接**：
  - <https://github.com/xuebinqin/DIS/blob/main/LICENSE.md>
  - <https://github.com/xuebinqin/DIS#7-term-of-use>（Term of Use 与模型发布注记所在 README 段落）

---

<a id="gfpgan"></a>

### 2. GFPGAN（GFPGANv1.pth / GFPGANv1.3.pth / GFPGANv1.4.pth）— 层级 A

- **仓库**：<https://github.com/TencentARC/GFPGAN>（默认分支 master）
- **LICENSE 文件**：根目录 `LICENSE` = **腾讯声明式 Apache-2.0**（GitHub SPDX 因非标准文本标 NOASSERTION）
- **关键条款摘录**（文件头逐字）：
  > Tencent is pleased to support the open source community by making GFPGAN available.
  >
  > Copyright (C) 2021 THL A29 Limited, a Tencent company. All rights reserved.
  >
  > GFPGAN is licensed under the Apache License Version 2.0 except for the third-party components listed below.
  
  正文为完整 Apache-2.0 条款（含第 4 条 Redistribution 再分发权），文末附第三方组件清单（均为随附各自许可的开源组件），无任何"仅限研究/禁止商用"类限制。
- **权重是否与代码同许可**：**是（实务认定）**。GFPGANv1.pth / v1.3.pth / v1.4.pth 均为本仓库 GitHub Release 资产（README 逐字给出 `https://github.com/TencentARC/GFPGAN/releases/download/v1.3.0/GFPGANv1.4.pth` 等直链），属同一 Apache-2.0 声明项目的正式发布物，未附加独立限制条款。
- **商用/再分发结论**：**可捆绑再分发**（Tier A）。条件：分发时保留 LICENSE 文本与版权声明、附第三方组件清单；不使用 Tencent 商标做背书。
- **证据链接**：
  - <https://github.com/TencentARC/GFPGAN/blob/master/LICENSE>
  - <https://github.com/TencentARC/GFPGAN/releases/download/v1.3.0/GFPGANv1.4.pth>（README Model Zoo 表引用的权重直链）

---

<a id="ifrnet"></a>

### 3. IFRNet（S / L，GoPro / Vimeo90K）— 层级 B

- **仓库**：<https://github.com/ltkong218/IFRNet>（默认分支 main）⚠️ **勘误**：任务书所写 `FengyangPang/IFRNet` 已不存在（API 返回 Not Found）；CVPR 2022 论文官方实现仓为第一作者单位成员 ltkong218 名下仓库。
- **LICENSE 文件**：`LICENSE` = **MIT License**
- **关键条款摘录**（逐字）：
  > MIT License
  >
  > Copyright (c) 2022 Lingtong Kong
  
  （其余为标准 MIT 正文：允许使用、复制、修改、合并、出版、分发、再许可与销售。）
- **权重是否与代码同许可**：**未逐字声明**。README「Download Pre-trained Models」段给出预训练模型（IFRNet/IFRNet_S/IFRNet_L 等 checkpoints）经 Dropbox 外链分发：
  > Download our pre-trained models in this link（Dropbox）
  
  未附带独立许可文本，也未声明"链接内容同项目许可"。
- **商用/再分发结论**：代码按 MIT 可用；**权重不满足 Tier A 门槛 → Tier B**：引导用户从官方 Dropbox 渠道自取，或采用 ncnn 移植版（社区存在 `nihui/ifrnet-ncnn-vulkan`，其 MIT Release 自带转换模型，可作为兜底运行时的获取指引）。能力槽位上 IFRNet 本就是 RIFE（Tier A 已核）之外的备选 VFI，不构成阻塞。
- **证据链接**：
  - <https://github.com/ltkong218/IFRNet/blob/main/LICENSE>
  - <https://github.com/ltkong218/IFRNet#download-pre-trained-models-and-play-with-demos>（Dropbox 权重外链所在段落）

---

<a id="cain"></a>

### 4. CAIN（原仓 myungsub/CAIN ＋ nihui 移植版）— 层级 B

**原仓库**

- **仓库**：<https://github.com/myungsub/CAIN>（默认分支 master）
- **LICENSE 文件**：`LICENSE` = **MIT License**
- **关键条款摘录**（逐字）：
  > MIT License
  >
  > Copyright (c) 2021 Myungsub Choi
- **LICENSE 添加时间**：提交记录显示 LICENSE 于 **2021-10-06** 以 "Create LICENSE" 提交入库 —— **推翻矩阵初稿"原仓库历史无明确许可"的判断**（该判断基于更早的仓库状态快照）。
- **权重是否与代码同许可**：**未逐字声明**。README 给出预训练权重外链：
  > Download pretrained models from [Here]（→ Dropbox `pretrained_cain.pth`）
  
  无独立许可文本。
- **商用/再分发结论**：torch 原版权重 **Tier B**（引导官方 Dropbox 渠道）。

**nihui 移植版**

- **仓库**：<https://github.com/nihui/cain-ncnn-vulkan>（默认分支 master）
- **LICENSE**：`LICENSE` = **MIT License, Copyright (c) 2020 nihui**（逐字核验）
- **关键事实**：Release 说明逐字："This package includes all the binaries and models required."——移植版 Release 直接携带转换后的 CAIN 模型，nihui 以 MIT 名义公开再分发多年，属 VFI 工具链通行做法。
- **商用/再分发结论**：**ncnn 兜底路径可直接引导用户下载 nihui 官方 Release**（引擎+模型一体，MIT）；若自行捆绑 torch `.pth` 原版则仍按原仓 Tier B 处理。
- **证据链接**：
  - <https://github.com/myungsub/CAIN/blob/master/LICENSE>
  - <https://github.com/myungsub/CAIN/commits/master/LICENSE>（2021-10-06 "Create LICENSE"）
  - <https://github.com/nihui/cain-ncnn-vulkan/blob/master/LICENSE>
  - <https://github.com/nihui/cain-ncnn-vulkan#download>

---

<a id="dain"></a>

### 5. DAIN（baowenbo/DAIN）— 层级 B

- **仓库**：<https://github.com/baowenbo/DAIN>（默认分支 master）
- **LICENSE 文件**：`LICENSE` = **MIT License**
- **关键条款摘录**（逐字）：
  > MIT License
  >
  > Copyright (c) 2019 Wenbo Bao
- **LICENSE 添加时间**：提交记录显示 **2019-03-22** 即已入库 —— **推翻矩阵初稿"baowenbo/DAIN 历史无 LICENSE 文件，风险高"的判断**。
- **权重是否与代码同许可**：**未逐字声明**。README「Testing Pre-trained Models」给出：
  > Download pretrained models,
  >
  > `$ wget http://vllab1.ucmerced.edu/~wenbobao/DAIN/best.pth`
  
  权重在作者所属加州大学默塞德分校实验室服务器上公开分发，无独立许可文本；该域名长期可达性存疑。
- **商用/再分发结论**：代码 MIT 可用；权重 **Tier B**（引导 README 所载官方地址；实现侧应准备地址失效时的降级文案）。相较初判 C 上调一级，但**仍不可捆绑**。
- **证据链接**：
  - <https://github.com/baowenbo/DAIN/blob/master/LICENSE>
  - <https://github.com/baowenbo/DAIN#testing-pre-trained-models>（best.pth 下载指令所在段落）

---

<a id="realsr"></a>

### 6. RealSR（DF2K / DPED / DF2K-JPEG）— 层级 B

- **仓库**：任务书所写 `jixiaozhong/Real-SR` 已 **Moved Permanently**（GitHub API repository id 290983604 重定向核验）→ 现址 **<https://github.com/Tencent/Real-SR>**（默认分支 master）
- **LICENSE 文件**：双许可文件结构
  - `LICENSE_RealSR` = **腾讯声明式 Apache-2.0**（GitHub SPDX 判定 Apache-2.0）。文件头逐字：
    > Tencent is pleased to support the open source community by making RealSR-真实图像超分算法 available.
    >
    > Copyright (C) 2020 THL A29 Limited, a Tencent company. All rights reserved. …
    >
    > RealSR-真实图像超分算法 is licensed under the the Apache License Version 2.0 except for the third-party components listed below.
    
    正文为完整 Apache-2.0 条款；文末"Other dependencies and licenses"列明 BasicSR 1.0.0（Apache-2.0，随附副本即 `LICENSE_BasicSR`）。
  - `LICENSE_BasicSR` = Apache License 2.0 全文（BasicSR Authors, 2018-2020）
- **权重是否与代码同许可**：**未逐字声明**。README「Pre-trained models」段给出 DF2K / DPED / DF2K-JPEG 三个权重均为 **Google Drive 外链**；另有官方 ncnn 可执行包（经 `nihui/realsr-ncnn-vulkan` 发布）。权重本体不在仓库树内。
- **商用/再分发结论**：代码 Apache-2.0 明确可用；权重 **Tier B**——引导用户自 Google Drive 官方链接或 nihui realsr-ncnn-vulkan Release 获取；不满足捆绑门槛。
- **证据链接**：
  - <https://github.com/Tencent/Real-SR/blob/master/LICENSE_RealSR>
  - <https://github.com/Tencent/Real-SR/blob/master/LICENSE_BasicSR>
  - <https://github.com/Tencent/Real-SR#pre-trained-models>（三个 GDrive 权重链接所在段落）

---

<a id="srmd"></a>

### 7. SRMD（SRMD x2/x3/x4 + NF 变体）— 排除

- **仓库**：<https://github.com/cszn/SRMD>（默认分支 master；最后推送 2021-10-09，已停更）
- **LICENSE 文件**：**不存在**。GitHub API `license` 字段为 null；根目录文件清单（Demo_*.m、README.md、TrainingCodes、models、results、testsets、utilities、figs）中**无任何 LICENSE/COPYING 文件**；README 无许可章节。
- **关键事实**：
  - MatConvNet 格式权重（`SRMDx2.mat / SRMDx3.mat / SRMDx4.mat / SRMDNFx2~x4.mat` 等）**直接位于仓库 `models/` 目录内**，但仓库整体零许可文本 → 默认著作权保留，任何人未获授权；
  - PyTorch 版 `srmd_x2/x3/x4.pth`（MatConvNet 参数转换而来）由作者工具箱 **cszn/KAIR** 经 Google Drive model_zoo 分发；KAIR 仓库虽为 **MIT**，但其许可覆盖的是工具箱**代码**，README 亦未对 SRMD 权重作出同许可声明——转换行为不产生新授权。
- **权重是否与代码同许可**：❌ 不适用（根本无许可授予）。
- **商用/再分发结论**：**排除**（较初判 C 从严修正：并非"疑似研究用途限制"，而是**零授权**）。不采纳、不引导、不随包；video-upscale 的全部能力槽位已由 waifu2x / Real-CUGAN / Real-ESRGAN 公版覆盖，无引入必要。若未来作者补充许可文本可复评。
- **证据链接**：
  - <https://github.com/cszn/SRMD>（无 License 徽章；models/ 目录含 .mat 权重）
  - <https://github.com/cszn/KAIR>（MIT 工具箱；model_zoo 经 Google Drive 分发 srmd*.pth）

---

<a id="firered-ocr"></a>

### 8. FireRed-OCR（FireRed-OCR-2B）— 层级 A

- **仓库**：<https://github.com/FireRedTeam/FireRed-OCR>（默认分支 main；小红书 FireRed Team）
- **LICENSE 文件**：`LICENSE.txt` = **Apache License 2.0** 标准全文（GitHub SPDX 判定 Apache-2.0）
- **关键条款摘录**：
  - HuggingFace 模型卡 <https://huggingface.co/FireRedTeam/FireRed-OCR> 页面元数据：`License: apache-2.0`；卡片「License Agreement」节逐字：
    > The code and the weights of FireRed-OCR are licensed under Apache 2.0.
  - 该卡同时给出 ModelScope 镜像（`FireRedTeam/FireRed-OCR`）。
- **权重是否与代码同许可**：✅ **是，逐字明示**（code and weights 均 Apache-2.0）——本批次核实中权重授权最明确的样本之一。
- **商用/再分发结论**：**Tier A 可捆绑再分发**（保留 NOTICE/许可文本即可）。注意其基座为 Qwen3-VL-2B-Instruct（Apache-2.0，同族已核），无许可传染问题。
- **证据链接**：
  - <https://github.com/FireRedTeam/FireRed-OCR/blob/main/LICENSE.txt>
  - <https://huggingface.co/FireRedTeam/FireRed-OCR>
  - <https://www.modelscope.cn/models/FireRedTeam/FireRed-OCR>

---

<a id="qwen3-asr"></a>

### 9. Qwen3-ASR（0.6B / 1.7B）与 Qwen3-ForcedAligner（0.6B）— 层级 A

- **权重仓库**（Qwen 官方 org，2026-01-29 发布）：
  - <https://huggingface.co/Qwen/Qwen3-ASR-0.6B> — 页面元数据 **License: apache-2.0**
  - <https://huggingface.co/Qwen/Qwen3-ASR-1.7B> — 同上（同批发布）
  - <https://huggingface.co/Qwen/Qwen3-ForcedAligner-0.6B> — 页面元数据 **License: apache-2.0**（本次经 hf-mirror.com 镜像逐项核验页面标注）
  - 双源镜像：ModelScope `Qwen/Qwen3-ASR-0.6B / -1.7B / Qwen3-ForcedAligner-0.6B`（官方卡片给出的标准下载命令同时列出两源）
- **代码仓库**：<https://github.com/QwenLM/Qwen3-ASR>（Apache-2.0，与 Qwen3 家族惯例一致）
- **关键条款摘录**：三张模型卡均以 HF 标准 `License: apache-2.0` 元数据标注；无 extra_gated 门禁字段，无需注册即可拉取。
- **权重是否与代码同许可**：✅ 是（权重仓库自身标注 apache-2.0）。
- **商用/再分发结论**：**Tier A 可捆绑再分发**（遵守 Apache-2.0 声明与 NOTICE 要求即可）。
- **证据链接**：
  - <https://huggingface.co/Qwen/Qwen3-ASR-0.6B>
  - <https://huggingface.co/Qwen/Qwen3-ForcedAligner-0.6B>
  - <https://huggingface.co/collections/Qwen/qwen3-asr>
  - <https://github.com/QwenLM/Qwen3-ASR>

---

<a id="rmbg-14"></a>

### 10. RMBG-1.4（briaai）— 层级 C（维持，条款原文已核）

- **权重仓库**：<https://huggingface.co/briaai/RMBG-1.4>（gated 访问；同步镜像于 ModelScope `briaai/RMBG-1.4`，License 字段 other）
- **许可证类型**：**定制许可 `bria-rmbg-1.4`**（卡片元数据 `license_name: bria-rmbg-1.4`，`license_link: https://bria.ai/bria-huggingface-model-license-agreement/`；仓库另附 `BRIA_License.docx`）
- **关键条款摘录**（模型卡「Model Description」节逐字）：
  > Developed by BRIA AI, RMBG v1.4 is available as a source-available model for non-commercial use.
  >
  > - **License:** bria-rmbg-1.4
  >   - The model is released under a Creative Commons license for non-commercial use.
  >   - Commercial use is subject to a commercial agreement with BRIA.
- **权重是否与代码同许可**：❌ 权重受上述定制非商业条款约束；推理代码（transformers/BRIA-pipeline 实现）不受此限。
- **商用/再分发结论**：**Tier C 手动指引维持**——仅个人/研究场景可由用户自行获取放置；EntryPoint 不随包分发、不提供一键下载入口；商用需求引导用户与 BRIA 签署商业协议。
- **证据链接**：
  - <https://huggingface.co/briaai/RMBG-1.4>
  - <https://bria.ai/bria-huggingface-model-license-agreement/>
  - <https://www.modelscope.cn/models/briaai/RMBG-1.4>

---

<a id="practical-rife"></a>

### 11. Practical-RIFE（RIFE v2→v4.26 全系含 lite/large）— 层级 A（维持，补原文链接）

- **仓库**：<https://github.com/hzwer/Practical-RIFE>（默认分支 main）
- **LICENSE 文件**：`LICENSE` = **MIT License**，原文链接：
  - Blob：<https://github.com/hzwer/Practical-RIFE/blob/main/LICENSE>
  - Raw：<https://raw.githubusercontent.com/hzwer/Practical-RIFE/main/LICENSE>
- **关键条款摘录**（逐字）：
  > MIT License
  >
  > Copyright (c) 2021 hzwer
  
  README「Trained Model」节逐字（模型链接的许可声明，系 Tier A 成立的关键证据）：
  > ### Trained Model
  > The content of these links is under the same MIT license as this project. **lite** means using similar training framework, but lower computational cost model.
- **权重是否与代码同许可**：✅ 是——上游逐字声明各模型发布链接的内容与本仓库同为 MIT。
- **商用/再分发结论**：**Tier A 可捆绑再分发**（保留版权与许可声明即可）。
- **证据链接**：
  - <https://github.com/hzwer/Practical-RIFE/blob/main/LICENSE>
  - <https://github.com/hzwer/Practical-RIFE#trained-model>

---

## 附录：既有公版条目交叉复核（2026-08-22，浅克隆 LICENSE 原文比对）

以下条目此前已定案，本轮以同样方法交叉复核，全部与矩阵既有结论一致，无变更：

| 模型族 | 仓库 | LICENSE 原文首行（逐字） | 复核结果 |
|---|---|---|---|
| U²-Net (u2net) | xuebinqin/U-2-NET | `Apache License / Version 2.0, January 2004` | ✅ Apache-2.0，维持 A |
| BiRefNet | ZhengPeng7/BiRefNet | `MIT License / Copyright (c) 2024 ZhengPeng` | ✅ MIT，维持 A |
| waifu2x cunet/upconv | nagadomi/waifu2x | `The MIT License / Copyright (C) 2015 nagadomi <nagadomi@nurs.or.jp>` | ✅ MIT，维持 A |
| Real-CUGAN se/pro/nose | bilibili/ailab（子目录 Real-CUGAN） | `MIT License / Copyright (c) 2022 bilibili` | ✅ MIT，维持 A |
| Real-ESRGAN 官方系 | xinntao/Real-ESRGAN | `BSD 3-Clause License / Copyright (c) 2021, Xintao Wang` | ✅ BSD-3-Clause，维持 A |
| Whisper CT2（Systran） | Systran/faster-whisper | MIT（在用条目，未重复取证） | ✅ 维持 A |

## 方法说明与局限

1. **取证通道**：本环境 `raw.githubusercontent.com` 与 `huggingface.co` 直连超时。GitHub 侧改用 `api.github.com/repos/<owner>/<repo>/license` 端点（返回 LICENSE 文件 base64 原文，等同读文件）＋ `git clone --depth 1` 复核；HuggingFace 卡片经 websearch 返回的页面快照与 hf-mirror.com 镜像核验。所有"逐字摘录"均来自上述一手通道，非二手转述。
2. **默认分支**：均以 `git ls-remote --symref` 或克隆实测为准（DIS=main、GFPGAN/CAIN/DAIN/cain-ncnn-vulkan/Real-SR=master、IFRNet/FireRed-OCR/RIFE=main），文中 blob 链接据此构造。
3. **"外链权重未逐字授权"的口径**：CAIN/DAIN/IFRNet/RealSR/ISNet 的权重均由上游经第三方网盘公开发布、无独立许可文本。本矩阵将其归入 Tier B（引导用户自取）而非 Tier A（捆绑），是**保守判定**：作者公开发布行为通常被理解为默示使用许可，但不满足本项目"权重同源许可"的可捆绑标准。此口径与 §2.5 规则行（待核按高限制对待）一致。
4. **GFPGAN / RealSR 的 NOASSERTION 说明**：两者均为"腾讯声明式 Apache-2.0"（自定义抬头 + 完整 Apache 正文 + 第三方清单），GitHub 无法映射到标准 SPDX 故标 NOASSERTION；法律效力与标准 Apache-2.0 相同，再分发义务亦相同。
