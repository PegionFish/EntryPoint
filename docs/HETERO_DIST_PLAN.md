# 异构计算落地 + 模块独立分发 — 设计方案与执行计划

> 版本：v5 | 日期：2026-08-22 | 状态：**待用户确认**
>
> v5 变更：SR/VFI 运行时路线定案——**厂商栈优先**（CUDA/ROCm = 官方 torch 权重；
> Intel = OpenVINO ONNX 路线），**ncnn-vulkan 降为通用兜底**（仅当厂商栈不可用时启用）；
> 权重按运行时声明多格式变体（.pth / onnx / ncnn），复用现有 [[models]] schema；
> 验证实验扩充至 E1–E8。
>
> v4 变更：① 视频超分与插帧回归为核心需求；② Waifu2x-Extension-GUI 模型甄别：
> 仅采纳公版权重，作者自训系列（-W2xEX 后缀）一律排除，引擎不取其重编译版；
> ③ RIFE v4.x 权重经上游核实为 MIT。
>
> v3 变更：去商店化——无商店/注册表运营，分发即 GitHub Release 挂标准压缩包，
> 用户自行下载导入或手动解压。
>
> v2 变更：废除自定义模块包格式（.epmod 方案撤销），模块分发一律采用标准压缩档案
> （zip / tar.gz）+ 发布侧 SHA256 校验和文件；完整性校验交给用户侧工具链（目标用户为
> poweruser / 发烧友）；服务端导入仅做安全解包校验，不引入专有格式概念。
>
> 定位约束：EntryPoint 为 LAN 部署的多媒体 AI 聚合工具（WebUI + API，NAS / 工作站常驻）。
> 研发阶段策略：**单仓推进（本 repo）**，功能验证通过后再拆分模块仓库与分发渠道。
> 执行模型：多子代理并行开发，编排规则见 §8。

---

## 1. 背景与目标

### 1.1 三大目标

| # | 目标 | 验收口径 |
|---|---|---|
| G1 | **异构计算跑通** | cuda / rocm / openvino 三后端各由 ≥1 个模块在本机完成真实推理 E2E |
| G2 | **模块独立分发就绪** | 模块目录 ↔ 标准压缩包（zip/tar.gz）双向转换 + 导入运行闭环在单仓内验证通过 |
| G3 | **模块组合扩充** | 新增 4–6 个多媒体能力模块，覆盖 ASR 对齐 / OCR / 抠图 / **视频超分与插帧（核心需求）** |

### 1.2 本机硬件与就绪状态（2026-08-22 实测）

| 设备 | 后端 | 就绪状态 |
|---|---|---|
| RTX 5090 D 32GB | cuda | ✅ 已验证（large-v3 GPU 转写 E2E） |
| AMD RX 7900 XTX (gfx1100) | rocm | ✅ 驱动栈就绪（/opt/rocm、rocminfo 可见）；无模块实测 |
| Intel Arrow Lake iGPU | openvino (GPU.0) | ⚠️ 设备可检出；无模块消费 |
| Intel Core Ultra 200 NPU (`/dev/accel0`) | openvino (NPU.0) | ⚠️ 节点存在；需在 venv 内验证 OV 运行时可见性 |
| CPU (Ultra 7 270K Plus) | cpu | ✅ 兜底路径已验证 |

### 1.3 排除项（本期不做）

- ComfyUI / sd-webui 类平台型应用接入（与平台自身定位重叠）
- Wan2.2-Animate / LatentSync / LTX2.3 等重型视频生成（不符合 NAS 常驻服务模型）
- NVIDIA Maxine（专有许可，不可再分发）
- 跨设备自动切分调度、OpenVINO IR 自动转换（沿用 PACK_UNIFY_PLAN 排除决策）

---

## 2. 分发架构（研发期单仓，拆分后生效）

### 2.1 两阶段拓扑

```
阶段一（现在，单仓）                     阶段二（验证后，拆分）
EntryPoint/                             EntryPoint/            ← 主仓只发核心套件
├── crates/…                            ├── crates/…
├── modules/                            └── dist/（核心套件产物）
│   ├── <官方维护的 5 个现有模块> ↘      ep-module-repos/       ← 每模块一个独立 git 仓库
│   ├── <新增模块同仓开发>     ──拆分→   │   ├── ep-mod-faster-whisper/
│   └── （目录布局即拆分单元）           │   ├── ep-mod-qwen3-asr/
                                        │   └── …
                                        发布物：GitHub Release 挂 <module-id>-<ver>.zip / .tar.gz + SHA256SUMS.txt
                                        用户流程（三选一）：
                                        a. WebUI 导入页上传压缩包（自动校验落位）
                                        b. 自行下载压缩包 → WebUI/API 上传导入
                                        c. 完全手动：解压到 modules/<id>/ → 刷新识别（poweruser 首选）
```

**单仓期的纪律（为拆分降成本）：**
1. 每个 `modules/<id>/` 目录必须自包含（module.toml + adapter.py + requirements*.txt + README），不引用兄弟模块文件；
2. 模块内禁止硬编码部署根路径，一律走 `{ROOT}/{MODULE_DIR}/{MODEL_DIR}` 变量；
3. 主仓 `modules/` 中现有 5 个模块视为"待拆分存量"，本期不迁移但新模块按可拆分标准编写；
4. 打包/导入机制本期实现并在单仓内闭环验证（§2.3），拆分仅改变 ZIP 的托管位置。

### 2.2 分发载体：标准压缩档案（无自定义格式）

**原则：模块目录本身就是分发单元。** `modules/<module-id>/` 打成 zip 或 tar.gz 即为
发布物；反之，任何"根部含一个 `module.toml`"的标准压缩包都可被平台导入。
不引入专有扩展名、专有清单或专用工具链。

**发布纪律（模块仓库维护者执行）：**

```
faster-whisper-1.4.0.zip            ← 内容即 modules/faster-whisper/ 目录本身
faster-whisper-1.4.0.tar.gz         ← 同内容第二格式，按平台习惯二选一发布亦可
SHA256SUMS.txt                      ← 全部发布物的 sha256 清单（标准格式，sha256sum -c 可验）
```

- 包内布局 = MODULE_SPEC §1 模块目录结构，`module.toml` 位于包根（或唯一一级目录下）；
- 完整性：发布侧出 SHA256SUMS.txt；用户自行 sha256sum/md5sum 校验——平台不做私有校验格式；
- 服务端导入时的解包安全校验（禁绝对路径/`..`/符号链接等）复用模型管理浏览器上传的
  既有实现，属于通用安全措施而非格式约定。

**与 `.epzip` 整合包的关系：**
`.epzip`（模型+管线配置包）为已交付的历史功能，维持现状不动、不扩散；本期一切新工作
只基于标准档案。整合包未来是否向标准档案收敛，留待拆分阶段另行评审，不在本期范围。

### 2.3 平台侧新增 API（研发期即可用）

| API | 行为 |
|---|---|
| `POST /api/modules/import` | 上传 zip/tar.gz → 安全解包校验（复用模型上传链路）→ 落位 `modules/<id>/`；已存在同 id 模块时按 manifest version 判断：仅允许升级，降级/同版拒绝并报错 |
| `GET /api/modules/export/<id>` | 将 `modules/<id>/` 打包为 zip 下载（排除运行期产物），供用户迁移分享；同步产出 SHA256SUMS.txt |
| `POST /api/modules/import-url` | 从 URL（如 GitHub Release 直链）拉 zip/tar.gz 导入（复用模型下载器的代理/进度链） |

> 无商店、无注册表运营：本工具为开源项目，不具备商店基础设施。模块获取 =
> 用户自行从 GitHub Releases 下载 + 校验 + 导入/手动解压。

导入信任模型从简（poweruser 定位）：导入响应回显 manifest 摘要（id/version/license/backends）
+ sha256，由用户自行判断来源可信；平台不维护激活状态机。手动解压路径（§2.1 用户流程 c）
始终一等公民——目录扫描发现新模块即纳入管理。

### 2.4 模型获取三级策略（许可证驱动）

模块的权重**一律不随模块压缩包分发**，按上游许可证分级：

| 层级 | 定义 | 用户体验 | 实现载体 |
|---|---|---|---|
| **Tier A 可捆绑** | 上游许可允许再分发（BSD/MIT/Apache 且权重同源许可） | 导入后一键"随包下载"或整合包 bundle | `[[models]]` 常规声明（HF/MS/URL 三源已有） |
| **Tier B 引导下载** | 许可允许分发但需署名/限定用途，或有更稳的官方源 | WebUI 模型页正常展示下载按钮 + 许可提示 | `[[models]]` + 新字段 `[models].license_note` |
| **Tier C 手动指引** | 不可再分发 / 注册制下载 / 专有 | 模块卡片显示"手动安装指引"，用户自行放置目录后点"扫描导入"（本地导入路径已有） | `[[models]] guide_url` 或 README 章节 + `/info` 返回指引文本 |

### 2.5 许可证合规矩阵（初始判定，WS-G 代理逐项核实）

| 模型族 | 上游许可 | 初判层级 | 备注 |
|---|---|---|---|
| U²-Net (u2net) | Apache-2.0 | A | xuebinqin |
| ISNet (isnet-general-use) | 待核 | B→A? | DIS 仓库代码与权重许可需分别核 |
| BiRefNet | MIT | A | ZhengPeng7 |
| GFPGAN 1.4 | 待核 | B | Apache/BSD 需核权重条款 |
| RMBG-1.4 (briaai) | 非商业 | C | 仅个人/研究场景引导 |
| Qwen3-ASR / ForcedAligner | Apache-2.0（Qwen 系惯例） | A | 以仓库 LICENSE 核实为准 |
| FireRed-OCR | Apache-2.0（待核） | A | FireRedTeam |
| Whisper CT2 权重（Systran） | MIT | A | 已在用 |
| **waifu2x** cunet/upconv noise0-3 | MIT | A | nagadomi；包内命名与上游一致 |
| **Real-CUGAN** se/pro/nose up2x-denoise0-3x | MIT | A | bilibili/ailab；公版 |
| **Real-ESRGAN 官方系** x4plus / x4plus-anime / animevideov3 x2-x4 / general-x4v3(+wdn) / animevideo-xsx2/xsx4 | BSD-3-Clause | A | xinntao；仅取官方名，排除 -W2xEX 自训 |
| **RIFE v2→v4.26 全系（含 lite/large）** | **MIT（已核实）** | A | Practical-RIFE 声明"链接内容同项目 MIT"；DeepWiki 复核"所有 RIFE 模型 MIT，可自由再分发" |
| IFRNet S/L GoPro/Vimeo90K | 待核 | B | FengyangPang/IFRNet 许可核实中 |
| CAIN | 待核 | B | 原仓库历史无明确许可，nihui 移植版注明模型归原作者 |
| DAIN | 待核 | C | baowenbo/DAIN 历史无 LICENSE 文件，风险高 |
| RealSR DF2K | 待核 | B | jixiaozhong/Real-SR 许可核实中 |
| SRMD x2/x3/x4 (+nf) | 待核 | C | 疑似研究用途限制 |
| W2xEX 作者自训系列（Omni-* / Photo-* / Anime-HQ / Universal-Fast / AnimeVideo-Mini 等） | 不明（作者自训产物） | **排除** | 包内 HTMLs/EsrganModelIntro 文档自证为作者训练；不采纳、不引导下载 |

> 规则：凡"待核"条目，在核实完成前一律按更高限制层级对待；核实结论回写本表并附证据链接。
> 引擎侧：ncnn-vulkan 各引擎均为 nihui 维护的 MIT 项目——一律采用上游官方发布二进制或
> 自行构建，**不使用 W2xEX 重编译版**（其 exe 带 `_W2xEX` 后缀，属作者源码级改动）。

---

## 3. 模块选型清单（结合 AI_Applications 存货 + Waifu2x 公版甄别）

### 3.1 新增/变更模块一览

| 模块 id | 来源 | 技术栈 | backends | 模型层级 | 优先级 |
|---|---|---|---|---|---|
| `faster-whisper`（增强） | 现有模块 + whisperx-offline 权重库导入 | CTranslate2 | cuda, **rocm**(新), cpu | A（Systran MIT） | P0 |
| `rembg`（增强） | 现有模块 | ONNX Runtime | cpu, **openvino**(新) | A（u2net）/B(isnet) | P0 |
| `onnx-matting`(新) | FaceFusion 包 ONNX 提炼（birefnet_general/portrait） | ONNX Runtime 多 EP | cpu, cuda, openvino, (directml) | A（BiRefNet MIT） | P1 |
| `qwen3-asr`(新) | Qwen3ASR/qwen-asr-0.6B/1.7B + ForcedAligner | torch + qwen_asr | cuda, cpu（rocm 观察） | A | P1 |
| `firered-ocr`(新) | FireRed-OCR | torch transformers | cuda, cpu | A（待核） | P2 |
| `video-upscale`(新，核心) | 公版 Real-ESRGAN / waifu2x / Real-CUGAN | **torch(cuda/rocm) 主 + OV-ONNX(intel) + ncnn-vulkan 兜底** | cuda, rocm, openvino, vulkan(备选), cpu | A（BSD-3/MIT） | **P0** |
| `video-interp`(新，核心) | 公版 RIFE 全系（MIT 已核实）/ IFRNet | 同上分层策略 | cuda, rocm, openvino, vulkan(备选), cpu | A（RIFE）/ B（IFRNet 待核） | **P0** |

> 视频超分与插帧为核心需求。运行时分层策略（用户定案）：**厂商栈优先**——
> CUDA/ROCm 走官方 torch 权重（xinntao .pth、RIFE .pkl，各自原生推理路径），
> Intel iGPU/NPU 走 OpenVINO ONNX 路线；**ncnn-vulkan 仅作兜底**——覆盖厂商栈
> 不可用、驱动残缺的机器。权重按运行时声明多格式变体（.pth / onnx / ncnn param+bin
> 各为独立 `[[models]]` 条目），adapter 按 backend 选载——复用现有 schema，零扩展。

### 3.2 Waifu2x-Extension-GUI 模型甄别结论（2026-08-22 实测）

**甄别方法**：包内各引擎 `models*/` 目录逐文件比对上游官方发布命名；作者自训模型
由包内文档 `HTMLs/EsrganModelIntro` 自证。

**✅ 公版采纳（文件命名与上游一致，能力槽位全覆盖）**

| 能力槽位 | 采纳的公版 | 上游 |
|---|---|---|
| 二次元 SR | waifu2x cunet/upconv noise0-3、Real-CUGAN se up2x-denoise0-3x | nagadomi (MIT)、bilibili (MIT) |
| 照片/通用 SR | realesrgan-x4plus(-anime)、realesr-general-x4v3(+wdn) | xinntao (BSD-3) |
| **视频 SR（核心）** | RealESRGANv2-animevideo-xsx2/xsx4、realesr-animevideov3-x2/x3/x4 | xinntao (BSD-3) |
| **插帧 VFI（核心）** | RIFE v2→v4.26 全系含 lite/large（contextnet/flownet/fusionnet 命名与 nihui 发布一致） | hzwer Practical-RIFE (**MIT 已核实**) |
| 备选 VFI | IFRNet S/L GoPro/Vimeo90K | FengyangPang (待核) |

**❌ 作者自训排除（-W2xEX / -W4xEX 后缀，包内文档自证为作者训练产物）：**
Omni-MiniV2 / Omni-Smallv2 / Omni-TurboV1.5 / Universal-FastV2-W2xEX、
Anime-HQ-W4xEX、Photo-HQ-W4xEX / Photo-Small-W2xEX / Photo-Conservative-x4、
AnimeVideo-MiniV1.8-W2xEX。对应能力槽位一律以上述公版替代，不导入改版。

**⚠️ 引擎二进制同样不取：** 包内全部 exe 带 `_W2xEX` 后缀 = 作者源码级重编译
（ncnn 引擎本身 MIT，但为避免引入未审计改动），统一使用 nihui 上游 Release
（Windows 官方构建 + Linux 构建脚本）或自行交叉编译。

**选型结构借鉴（非搬运）：**
1. 画质-速度阶梯：HQ / Balanced / Fast 多档 → `[[models]]` 多变体 + `vram_estimate_mb` 差异化，NAS 低端机可跑；
2. 场景分轨：photo / anime / anime-video 三轨 → 变体 tags 分类沿用；
3. 视频专用轻量模型优先：realesr-animevideov3 系列为 video-upscale 默认变体；
4. SR × VFI 组合补全视频增强链路 → 与既有降噪/ASR/字幕组成"老片修复""动漫补帧"DAG 样板管线。

### 3.3 直接导入的存量权重（零开发收益）

- `AI_Applications/whisperx-offline/models/`：11 个 Systran CT2 权重（tiny→large-v3 含 .en）→ 通过 WebUI 本地导入成为 faster-whisper 变体库，同时给 rocm 验证提供轻量模型（base/small 秒级冒烟）。
- FaceFusion 包内其余 ONNX（gfpgan/gpen 人脸修复、2dfan4 关键点等）→ 登记为后续 face 类模块素材，本期只取 matting 两枚。

**开发期权重使用纪律：**
- 可直接从 AI_Applications / Waifu2x 包 **cp 公版权重**到 `models/` 验证可用性
  （`models/` 已在 .gitignore，不会入库）；
- 作者自训改版（-W2xEX 系）禁止 cp 入工作流，防止误绑定变体；
- 发布前门禁：`git status` 确认零权重文件被跟踪；模块压缩包内永不携带权重。

---

## 4. 平台侧机制缺口（Rust 工作项）

| # | 工作项 | 内容 | 依据 |
|---|---|---|---|
| M1 | 模块导入/导出 | §2.3 三个 API；zip/tar.gz 安全解包（复用模型上传链路）；版本比较与落位规则 | 新增 |
| M2 | `requirements_by_backend` 消费 | 按 manifest 当前 backend 选择依赖文件；缺省回退 requirements.txt | MODULE_SPEC §2.6 schema 已冻结 |
| M3 | 分后端 venv | 目录演进 `runtime/venvs/<module>--<backend>/`；`.ep_deps_hash` 输入加入 backend 名；旧单 venv 兼容读取 | MODULE_SPEC §3.1 演进方向 |
| M4 | Vulkan 备选后端检测 | `ComputeBackend::Vulkan` + vulkaninfo 探测器；**优先级置于 openvino 之后、cpu 之前（备选位）**——仅当模块声明的厂商栈均无可用设备时由调度器兜底选中；MODULE_SPEC §2.3 词表同步升版 | 新增 |
| M5 | WebUI 导入体验 | 导入入口（上传 / URL）+ 手动解压路径引导文案 + Tier-C 手动指引展示 + 模块卡片后端兼容徽章 | 新增 |
| M6 | openvino 设备名注入复核 | 确认 `OPENVINO_DEVICE={device_name}` 注入链路对 `GPU.0`/`NPU.0` 取值正确，adapter 侧 `EP_BACKEND`/`EP_DEVICE` 语义文档化 | process.rs 已有注入点 |

---

## 5. 异构计算验证计划（G1 的最小证明集）

| 实验 | 后端×设备 | 模块 | 通过标准 |
|---|---|---|---|
| E1 | rocm × RX 7900 XTX | faster-whisper + base/small CT2 | `HIP_VISIBLE_DEVICES` 注入生效；CTranslate2-ROCm wheel 装载成功；转写结果与 cuda 路径一致（容差内） |
| E2 | openvino:NPU.0 × /dev/accel0 | rembg u2net（~168MB） | venv 内 `openvino` runtime 枚举出 NPU；抠图输出 PSNR 合理；设备利用率非零 |
| E3 | openvino:GPU.0 × Arrow Lake iGPU | rembg isnet 或 onnx-matting birefnet | 同上，设备=GPU.0 |
| E4 | cuda 回归 | 任一新模块冒烟 | 不劣于现状基线 |
| E5 | 调度矩阵 | scheduler 单元 + 集成测试扩展 | 三后端混合清单下 assign 结果符合策略（LeastMemory/Single 指定 openvino 设备等） |
| E6 | rocm × RX 7900 XTX | video-interp（RIFE torch 路线） | HIP 下完成一段视频 2x 插帧；输出帧数符合倍率、无花屏 |
| E7 | openvino:GPU.0 × Arrow Lake iGPU | video-upscale（OV ONNX 路线） | iGPU 完成一次 SR 推理，输出与 cuda 基线 PSNR 合理 |
| E8 | vulkan 兜底 × 三卡 | video-upscale/interp ncnn 路线 | M4 探测器枚举与真机一致；**模拟禁用厂商栈后调度自动落 vulkan**，各卡完成一次 SR 与一次 VFI |

失败处置约定：任一实验失败 → 记录到 PROGRESS「已知限制」，不阻塞其他工作流；rocm/openvino 至少一项跑不通时，对应模块保留 backend 声明但标注 experimental。

---

## 6. 波次计划

```
W0 契约冻结（串行，1 波）
 ├─ 分发载体细则定稿：包内布局、版本比较、SHA256SUMS 约定（标准档案，无自定义格式）
 ├─ module.toml 增量字段定稿（[distribution] license_note/guide_url）+ requirements_by_backend 语义细则（M2/M3 边界）
 └─ 路径所有权表 + 各子代理任务书生成（§8）

W1 并行开发（最大并行度，7 条工作流同时开）★核心波次
 ├─ WS-A 平台机制：M1–M6（Rust + WebUI，含 Vulkan 后端检测）
 ├─ WS-B faster-whisper：requirements-rocm.txt + CT2-ROCm 验证脚本 + 权重变体清单扩充
 ├─ WS-C rembg/onnx-matting：ORT-OV EP 接入 + 新模块脚手架
 ├─ WS-D qwen3-asr：adapter + 双变体（0.6B/1.7B）+ Aligner 能力
 ├─ WS-E firered-ocr：adapter + 权重声明
 ├─ WS-F video-upscale/interp：三运行时分层实现（torch cuda/rocm + OV-ONNX + ncnn 兜底）+ 公版权重按运行时多变体声明
 └─ WS-G 许可证核实 + 文档：合规矩阵回填 + MODULE_SPEC/PACK_AUTHORING 增补

W2 集成（收敛）
 ├─ 编排者统一接线：workspace 成员、config 默认值、docs 索引、i18n 键
 ├─ E1–E8 实验执行（真机）
 └─ 全量测试 + clippy 零警告门禁

W3 分发闭环演练（仍在单仓）
 ├─ 每个模块 export zip → 移走原目录 → import 回装 → venv 构建 → 推理冒烟；
 │  同时验证"手动解压路径"：直接 tar 解包到 modules/ 后刷新识别
 └─ store 页面走查 + 部署包（build.sh server）回归

W4 收尾
 ├─ PROGRESS/README 更新、已知限制登记
 └─ 拆分预案评审（哪些目录原样迁出、git filter-repo 历史策略草案——仅文档，不动手）
```

依赖关系：W1 各流互不阻塞；WS-B/C/D/E/F 依赖 W0 契约但不依赖 WS-A（先用临时 manifest 字段开发，M2 落地后切换）；WS-A 的 M1 导入 API 在 W3 才被强依赖。

---

## 7. 验收标准（DoD）

1. **异构**：E1–E8 全绿；`/api/devices` 三后端设备均能被 ≥1 模块实际占用推理；禁用厂商栈时 SR/VFI 自动落 vulkan 兜底；
2. **分发**：≥3 个模块完成 export→import→run 闭环 + 手动解压识别路径验证；导入安全校验单测覆盖 zip-slip/符号链接/重复条目/超大包/降级拒绝；SHA256SUMS.txt 由 export 流程自动产出；
3. **许可**：合规矩阵零"待核"残留；包内无 Tier-B/C 权重；
4. **质量**：全仓 `cargo test` 绿 + clippy 零警告（仓库既定标准）+ WebUI 无控制台错误；
5. **文档**：MODULE_SPEC 升版记录新增字段（vulkan 词表 + [distribution] 字段）；PACK_AUTHORING 增补模块包章节；PROGRESS 登记 E1–E8 结论。

---

## 8. 子代理并行开发规则（编排协议）

### 8.1 角色与所有权（互斥路径，杜绝编辑冲突）

| 工作流 | 独占写路径 | 只读路径 |
|---|---|---|
| WS-A | `crates/**`、`config/`、`crates/ep-webui/frontend/src/**` | docs/*（规范）、modules/*（清单结构参考） |
| WS-B | `modules/faster-whisper/**`、`scripts/hetero/whisper-rocm/*` | compute 检测器源码 |
| WS-C | `modules/rembg/**`、`modules/onnx-matting/**` | 同上 |
| WS-D | `modules/qwen3-asr/**` | PIPELINE_SPEC（产物协议 §5） |
| WS-E | `modules/firered-ocr/**` | ADAPTER_API |
| WS-F | `modules/video-upscale/**`、`modules/video-interp/**`、`reports/ws-f-engine-choice.md` | MODULE_SPEC |
| WS-G | `docs/**`、`reports/license-matrix.md` | 全仓只读 |
| 编排者 | Cargo.toml workspace 成员表、`config/app.toml` 默认段、README/PROGRESS 索引行、跨流冲突仲裁 | — |

规则：
- **R1 所有权互斥**：子代理越界写 = 任务书违规，编排者在集成窗口统一搬运/改写；
- **R2 契约先行**：跨流依赖一律指向 W0 冻结的 schema 文件，禁止口头约定；
- **R3 自包含任务书**：每个子代理 prompt 必须内联：背景摘要、规范文件绝对路径清单、交付物、验收命令（如 `cargo test -p ep-core compute::` / `python modules/x/tests/smoke.py`）、禁改路径；
- **R4 先侦察后动手**：复杂流（WS-A/F）先派 explore 型子代理出侦察报告，再派实现代理，两步都并行化；
- **R5 并行批次**：无依赖的 spawn 调用放在同一消息批量发出；重 IO 工作（venv 构建、权重解包）错峰调度防磁盘争用；
- **R6 验证自证**：子代理交付前必须在任务书指定命令上全绿并贴输出摘要；集成代理只信命令输出不信转述；
- **R7 升级而非修补**：发现契约缺陷 → 报告编排者修订 W0 文档并广播，禁止单方绕过；
- **R8 波次门禁**：W1 → W2 需全部工作流交付且各自验证绿；W2 门禁 = 全仓测试 + clippy 零警告；任何门禁不过则退回对应流修复后重跑门禁。

### 8.2 子代理任务书模板（编排者照抄填充）

```
【角色】你是 EntryPoint 项目的 <WS-x> 实现代理…
【上下文】必读：docs/MODULE_SPEC.md §2/§5、docs/HETERO_DIST_PLAN.md §<相关节>、
          参考实现 modules/rembg/adapter.py…
【独占路径】… 【禁改】…
【交付物】1) … 2) …
【验收】逐条命令 + 预期输出形态
【汇报格式】改动文件清单 / 测试输出摘要 / 未决问题 / 给编排者的契约反馈
```

### 8.3 并行度预算

- W1 同时活跃 ≤7（六实现流 + 一文档流）；每流内部还可再分侦察/实现双代理；
- 机器资源瓶颈：uv/torch 装环境属 IO 密集，WS-B/C/D/E 的 venv 构建步骤由编排者排队触发；
- 会话上下文管理：长流用 task_id 续会话，避免重复投喂上下文。

---

## 9. 决策点

### 已定案（用户确认，不再开放）

| 议题 | 结论 |
|---|---|
| 分发格式 | 标准压缩档案（zip/tar.gz）+ SHA256SUMS，无自定义格式 |
| 商店/注册表 | 不做（开源项目无运营基础设施），GitHub Release + 手动导入/解压 |
| 权重分发 | 一律不随包；开发期本地 cp 公版权重验证，发布不带任何第三方权重 |
| Waifu2x 系 | 仅采纳公版；作者自训（-W2xEX 系）与其重编译引擎排除 |
| SR/VFI 运行时 | 厂商栈优先（CUDA/ROCm=torch、Intel=OpenVINO ONNX），ncnn-vulkan 仅兜底 |
| NVIDIA Maxine | 排除（专有许可） |

### 待拍板

| # | 决策 | 建议 |
|---|---|---|
| D1 | Vulkan 兜底是否随首版交付 | 随首版：同一 adapter 内按 backend 分支逻辑简单，驱动残缺机器开箱即用；若工期紧可 W2 后补（厂商栈三卡已覆盖本机全部设备） |
| D2 | qwen3-asr 是否纳入 rocm 尝试 | 否，cuda+cpu 起步，rocm 待 CT2 经验后再评估 torch-rocm |
| D3 | Tier-B 权重的"许可提示"交互深度 | 卡片角标 + 详情页一行声明即可，不做阻断式弹窗 |
| D4 | 现有 5 模块是否本轮一并补 requirements_by_backend | 仅动 faster-whisper 与 rembg，其余留到拆分前统一过一遍 |
