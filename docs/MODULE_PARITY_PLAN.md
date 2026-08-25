# 模块功能对齐计划（AI_Applications Parity）— 设计方案与多子代理并行执行计划

> 版本：v2（已确认） | 日期：2026-08-26
> 依据：2 路并行侦察报告（音频/ASR/TTS 5 应用、OCR/图像/视频 12 应用）+ 本机权重与模块参数面盘点。
> 目标：**EntryPoint 已导入的 11 个模块**逐一对齐其源 WebUI 应用开放的功能面。
> 状态：**决策点 §10 Q1–Q5 已确认（全部推荐项），执行中**（W0 契约冻结完 → W1 七代理并行 → W2 权重 → W3 集成）。
> 执行者：多子代理并行开发模式（规程见 §5）。

---

## 1. 背景与目标

用户诉求：AI_Applications/ 下各 WebUI 已开放的功能，EntryPoint 已导入模块应具备同等能力——「逐模块对齐、不降级」。

### 1.1 目标

| # | 目标 |
|---|---|
| P1 | 11 个已导入模块的**参数面**对齐源应用（参数 schema 扩展，前端表单自动获得） |
| P2 | 能力面补齐：源应用有、模块没有的能力（如 qwen3-tts 三模、faster-whisper 11 变体、paddleocr PP-Structure 文档理解） |
| P3 | 权重对齐：源应用现成权重（whisper 11 款/rembg 6 款/qwen3-tts 1.7B 三件套等）可导入复用，不重新下载 |
| P4 | 后端 API 形状不变：仍走 `/api/execute/single` + 退化 DAG，新参数只是 schema 扩展 |
| P5 | 回归安全：现有任务/直跑/管线/生命周期全链路零破坏 |

### 1.2 排除

- **不新增模块**：IOPaint / FaceFusion / Sam3 / SoulX / LatentSync / LTX / Wan / ComfyUI-aki / sd-webui-aki / LLM-Translator 等**未导入**的源应用不做（清单见 §8 backlog）；
- 流式能力（Qwen3-ASR-Stream 的 vLLM 流式、TTS 流式输出）——需 vLLM 基础设施，不在本计划；
- ComfyUI/A1111 的 SD 生图参数面（模块未导入，无对应 EntryPoint 模块）；
- 不动执行引擎/端口协议，仅模块 manifest + adapter 层。

---

## 2. 现状盘点（事实源）

### 2.1 EntryPoint 已导入模块 × 源应用映射

| 模块 | 源应用 | 现有能力 | 源应用功能面（侦察确认） | 差距 |
|---|---|---|---|---|
| faster-whisper | faster-whisper-offline | transcribe | 11 模型变体（×.en）、dev/精度选择、SRT 输出 | 变体 3/11；缺 precision、SRT 产出 default |
| qwen3-asr | Qwen3ASR ×2 包 | transcribe + align | 0.6B/1.7B+aligner、30 语言、SRT 文本、词级 JSON、context 热词 | 已有 context/align；缺 SRT 输出形态、语言提示增强 |
| qwen3-tts | Qwen3TTS | synthesize | 1.7B VoiceDesign/Base(克隆)/CustomVoice（9 音色）、参考音频、instruct 情绪、11 语言 | **能力缺口大**：克隆/音色/情绪全缺；实现 0.6B 单模 |
| deep-filter | ai_sound_denoise | denoise | atten-limiting / pf 后置滤波 / SNR 阈值 ×3 / reduce-mask / 延迟补偿 | 已有 attenuation+min_db；缺 pf/阈值/reduce-mask |
| firered-ocr | FireRed-OCR | recognize | 单图/多图/PDF（DPI 150–600）、Markdown 输出流式 | 缺 PDF 输入、多图批、dpi |
| paddleocr | PaddleOcrPPStructureV3 | recognize (文本+coords) | **PP-StructureV3 文档理解**：版面/表格结构/公式 LaTeX/图表/方向/去扭曲、MD 输出 | **能力面差距最大**：纯 OCR vs 文档理解 |
| rembg | RemBg | remove_bg | 6 模型、alpha-matting、post-process | 已有 alpha/post_process（参数）；**变体 3/6**，缺 isnet-anime/u2netp/u2net_cloth/u2net_human |
| birefnet | RemBg/FaceFusion 的 birefnet 系 | matte | birefnet-general/portrait | 已对齐（2/2 含 model 覆盖） |
| animevideo | —（AI_Applications 无此源） | upscale (xsx2/4) | — | 无源应用，保持现状 |
| realesr | sd-webui Extras / IOPaint realesrgan | upscale (视频) | realesr-general-x4v3 / -x2x3 / ncnn | 已对齐（target_preset/tile）；缺 scale 映射补充（P3 backlog） |
| rife | video 插帧类 | interpolate | — | 保持现状 |

### 2.2 权重可用性（本机 AI_Applications 现成可复用）

| 源 | 权重 | 数量 | 备注 |
|---|---|---|---|
| faster-whisper-offline/models/ | Systran tiny/base/small/medium/large-v1/v2/v3 ×.en | 11 | HF snapshot 格式，可直接 folder 导入 |
| RemBg/models/ | isnet-anime/u2netp/u2net_cloth_seg/u2net_human_seg/u2net/isnet-general | 6 onnx | 缺 birefnet（已有 models/） |
| Qwen3TTS/models/ | Qwen3-TTS-12Hz-1.7B-{VoiceDesign,Base,CustomVoice} | 3 | 克隆/音色/情感三个模型 |
| Qwen3ASR/qwen-asr-1.7B/ | ASR-1.7B + ForcedAligner-0.6B | 2 | 变体已声明（module.toml 有），缺权重下载 |
| PaddleOcrPPStructureV3 | PP-StructureV3 全链路 13 子模型（layout/region/doc_ori/ocr_det/rec/formula/chart/table_*） | 子模型 | 需按 adapter 需求导入；目录结构需摸清 |

### 2.3 adapter 规模

11 个 adapter.py，400–711 行（平均 ~520）。参数面扩展是增量式（新 param 读入 + 透传底层调用），不动执行协议。

---

## 3. 差距 → 任务映射（8 项对齐工作）

| # | 模块 | 对齐内容 | 类型 |
|---|---|---|---|
| A1 | qwen3-tts | **三模能力**：VoiceDesign（text+语言+instruct 描述）/ Base 克隆（参考音频 + 可选参考文本，零样本 x-vector → ICL）/ CustomVoice（9 音色名 + 风格 instruct）；capability 划分：`synthesize`（照旧，default 声音设计）+ `clone_voice` + `custom_voice`；语言 11 项枚举；参数：instruct/spk_id/ref(路径或输入文件)/ref_text/x_vector_mode | manifest+adapter+变体模型 |
| A2 | paddleocr | **新增 `doc_understand` 能力**：PP-StructureV3 文档→Markdown（输入 image/pdf；开关 doc_orientation/unwarp/table/formula/chart 各级；输出 md/json 结构树；DPI）——保留原 recognize（文本+坐标）能力 | manifest+adapter+权重导入 |
| A3 | faster-whisper | 补充 8 变体（tiny/base/en/大 v1/v2）；precision 参数（fp16/int8 自动锁 CPU）；不破坏现有 | manifest+权重导入 |
| A4 | qwen3-asr | SRT 输出形态（timestamps 时输出 .srt 便捷产物 + word-level JSON 保持）；语言提示增强（30 语言枚举注入）；变体权重接线 1.7B | manifest+adapter |
| A5 | rembg | 补 4 变体权重（u2netp/human_seg/cloth/isnet-anime）；变体 schema 联动 alpha_matting（部分模型不支持时自动降级提示） | 权重导入+manifest |
| A6 | deep-filter | 补参：`post_filter`、`pf_beta`、`min_db_thresh`、`max_db_erb_thresh`、`max_db_df_thresh`、`reduce_mask`、`compensate_delay`（同名 CL） | adapter+manifest |
| A7 | firered-ocr | 新增 `recognize_pdf` 能力：PDF 输入 + dpi（150–600）+ 单图多图统一批；输出 md 文件产物 | adapter+manifest |
| A8 | birefnet/realesr/animevideo/rife | 参数面已达源（birefnet 2/2、realesr 档位、rife multiplier）；无工作 | 仅验证 |

---

## 4. 设计要点

### 4.1 参数 schema 与前端

- 新参数一律走 manifest `[interface.capabilities.params]`，前端「快速调用页」与模块页直跑抽屉**数据驱动自动生成**，零前端改动；
- select 型用 `type="select"` + `options`（现有先例）；数值型带 min/max/step；
- 可选输入（如 `ref_audio` 参考音频路径）：文本型参数（服务器路径，沿用 input_path 先例）+ 文件上传先经 `/api/upload/input` 回填——**不加新协议**。

### 4.2 Adapter 扩展约束

- adapter 继续遵守 ADAPTER_API 契约（POST /predict/<cap>，JSON/multipart 双形态，EP_* env）；
- 新增能力实现于已有 adapter.py（同 venv 内依赖现成）；不得新增 venv 结构；
- 变体（模型目录多份）由已有变体机制承载；`model`/`variant` 参数覆盖走 manifest `model` 参数先例（birefnet 已有）；
- 权重导入：经「模型管理 → 本地导入」从 `/home/bob/AI_Applications/**/models` 指定目录导入（.ep_meta 落 source="local"），或直接放 `<models>/<target_dir>`。

### 4.3 冻结契约（R0 产出，并行期不得偏离）

**A1 qwen3-tts**（capabilities 三个，原 synthesize 向后兼容扩展参数）：

```toml
# synthesize（保留）：text + language + instruct（声音描述）
instruct = { type = "string", default = "", description = "自然语言声音描述（VoiceDesign）" }
# 新 clone_voice：text + ref_audio 路径 + ref_text（可选）+ x_vector_mode
ref_audio = { type = "string", default = "", description = "参考音频服务器路径（上传回填）" }
ref_text = { type = "string", default = "", description = "参考文本；留空走 x-vector 零样本" }
# 新 custom_voice：text + spk_id（9 枚举）+ instruct（风格）
spk_id = { type = "select", options = ["Vivian","Serena","Uncle_Fu","Dylan","Eric","Ryan","Aiden","Ono_Anna","Sohee"], default = "Vivian" }
# 语言枚举（11）：chinese/english/german/italian/portuguese/spanish/japanese/korean/french/russian + auto
```

**A2 paddleocr `doc_understand`**：输入 image/pdf；`doc_orientation`(bool)、`doc_unwarping`(bool)、`table`(bool)、`formula`(bool)、`chart`(bool)、`dpi`(int 150-600, default 300)、`output_format`(select md/json)；原 recognize 不变。

**A2 落地契约（禁止猜测，必须按此）**：
- 源 zip：`/home/bob/AI_Applications/PaddleOcrPPStructureV3/PP-StructureV3-gpu-offline.zip`（5.7G，含 14 个子模型 + python312 环境）；
- W2 解压方案：`unzip -q PP-StructureV3-gpu-offline.zip -d /tmp/opencode/pps3/` → 得到 `pps3/PP-StructureV3-gpu-offline/`；
- 将其中 `models/`（14 子模型目录）拷为 `<models_cache>/paddleocr-pp-structure-v3/`（模块新增变体 `pp-structure-v3`，target_dir = `paddleocr-pp-structure-v3`），`doc_understand` 的 adapter 读 `EP_MODEL_DIR/`（即该 target_dir）下与 app.py 同口径的子目录名（`layout/region/doc_ori/ocr_det/ocr_rec/formula/chart/table_cls/table_wired*/table_wireless*/textline_orientation`，均为 **foldname**——注意 zip 内 chart 下的真实子目录是 `PP-Chart2Table`，其余为 HF 仓库名，adapter 需先 `ls` 一遍子目录再定装载，避免名称大小写/斜杠弄错）；
- adapter 可读**源 app.py**（解压后 `pps3/PP-StructureV3-gpu-offline/app.py`）来确认 PaddlePipeline 构造的具体参数（run_pipeline 的参数名/值）——必须以 app.py 为准，不得凭记忆写参数；
- 依赖注入：PP-StructureV3 需要 paddle !=2.x 的某些包；若模块 venv 已含 paddleocr 但缺结构模型依赖——A2 代理在报告「越权需求」节明示，INT 在 W3 统一处理（不改现有 requirements.txt 结构语义）。

**A1/qwen3-tts 落地契约**：
- 目标权重点位：`/home/bob/AI_Applications/Qwen3TTS/models/Qwen3-TTS-12Hz-1.7B-{VoiceDesign,Base,CustomVoice}/`（safetensors+Hf 配置全套，Base 模型用于 clone）；
- W2 导入为三个独立变体（`tts-voice-design`、`tts-base-clone`、`tts-custom-voice`），target_dir 沿用各目录名？**否**——统一维护在 `qwen3-tts` 模块的 [[models]] 变体表，target_dir 分别 `qwen3-tts-12hz-1.7b-voice-design` 等（与 `qwen3-tts-0.6b` 平行，W2 用本地导入 target_dir 参数指定）；
- adapter 的 clone_voice 能力：`x_vector_only_mode`（零样本，ref_text 空）→ `ep` 层传参数即可；10s 之内必须能加载（1.7B BF16 EP+GPU），CPU 保底要能跑（long）；若 adapter 调用超时/失败 → 回退 `synthesize` 并提示。


**A3 faster-whisper**：变体 +`precision`(select fp16/int8, default fp16, CPU 强制 int8)。

**A4 qwen3-asr**：`srt_output`(bool, default false) 走 output_format=srt 注入；语言枚举注入（LangEnums 30 项）。

**A5 rembg**：变体 +2（u2netp/human_seg/cloth/isnet-anime 共 6）；alpha_matting 语义：仅 general 系支持时行为、否则警告降级。

**A6 deep-filter**：
```toml
post_filter = { type = "boolean", default = false }
pf_beta = { type = "float", default = 0.02, min = 0, max = 2, step = 0.01 }
min_db_thresh = { type = "float", default = -15, min = -100, max = 0, step = 1 }
max_db_erb_thresh = { type = "float", default = 35, min = 0, max = 100, step = 1 }
max_db_df_thresh = { type = "float", default = 35, min = 0, max = 100, step = 1 }
reduce_mask = { type = "integer", default = 1, min = 1, max = 2 }
compensate_delay = { type = "boolean", default = false }
```

**A7 firered-ocr `recognize_pdf`**：`dpi`(150-600, default 300)；输出 md 文件（output_format=md 既有先例）；输入 pdf/image。

---

## 5. 多子代理并行开发规程

> 同 QUICK_RUN_PLAN §6 骨架，强化「契约先行 + 文件所有权互斥 + 波次推进 + 每波门禁」。

### 5.1 所有权矩阵（互斥写权限）

| 代理 | 独占写权限 |
|---|---|
| **INT**（集成） | 本文档、PROGRESS.md、api/types.ts（若无前端改动则不动）、i18n 键全量预留、static 产物 |
| **MOD-A1** | `modules/qwen3-tts/{module.toml,adapter.py,README.md}` + 测试 |
| **MOD-A2** | `modules/paddleocr/{module.toml,adapter.py,README.md}` + 测试 |
| **MOD-A3** | `modules/faster-whisper/{module.toml,adapter.py,README.md}` + 测试 |
| **MOD-A4** | `modules/qwen3-asr/{module.toml,adapter.py,README.md}` + 测试 |
| **MOD-A5** | `modules/rembg/{module.toml,adapter.py,README.md}` + 测试 |
| **MOD-A6** | `modules/deep-filter/{module.toml,adapter.py,README.md}` + 测试 |
| **MOD-A7** | `modules/firered-ocr/{module.toml,adapter.py,README.md}` + 测试 |
| **W8** | `models/`下权重导入（经本地导入 API 或直接放 target_dir，不写代码） |

规则：
1. **每模块一个代理**（7 个能并行），因为 11 个模块目录互不相交；
2. 共享文件路由：i18n 新键（`models:cap.*`）与 **前端 API 类型** 若无变更则零触碰；有需要时 `api/types.ts` 归 INT；
3. 不得创建新文件跨目录（如 `modules/common/`），模块内部自洽；
4. 若 adapter 需要共享辅助：**在模块内复制小段代码**（≤30 行），不跨模块依赖。

### 5.2 波次

```
W0（INT 串行）：契约冻结（§4.3 参数 schema/能力名全部落盘）+ 源权重校验 + 基线 commit
W1（7 代理并行）：MOD-A1..A7 按 §4.3 契约实现 module.toml+adapter.py，自测冒烟（本地 venv 跑 /health+小样本）
W2（W8 串行）：权重导入（AI_Applications → models/，逐个本地导入 API）
W3（INT 串行）：集成门禁（cargo:fmt/clippy/test 全绿；前端:tsc/lint/build 无影响则仅构建）+
  E2E：每模块真实小样本推理 + 快速调用页/管线页冒烟 + static 产物提交 + PROGRESS 记录
```

### 5.3 子代理 prompt 六段模板（照旧）

```text
【角色】W1-MOD-A1 实现代理（只实现本模块对齐，不做其他模块）
【目标】<§3 该模块行>
【独占文件】<§5.1 该行>
【冻结契约】<粘贴 §4.3 该模块的 schema/能力名原文>
【现状锚点】<模块目录 + 源应用目录路径清单>
【完成定义】adapter 内部单元冒烟（venv 内 import + /health + 小样本调用）；
  报告：改动文件+±行 / 冒烟输出尾部 / 偏离声明 / 越权需求
```

### 5.4 门禁

| 层 | 命令 |
|---|---|
| 每模块 | venv 内 `python -c "import adapter"` + `/health` 响应；
  后端 cargo fmt/clippy（若改后端）+ 运行任务冒烟 |
| 全量 | `cargo test --workspace` 、前端 `npm run build`（若前端变更）、i18n parity |
| E2E | 每模块走「快速调用页」上传样本→任务完成→产物下载（人工/脚本） |

### 5.5 提交纪律

- 每模块一 commit：`feat(parity)/mod-qwen3-tts: ...`、`feat(parity)/mod-paddleocr: ...`；
- W1 合并依序 INT 复跑门禁；W3 统一 static + PROGRESS。

---

## 6. Wave 分解与验收标准

| Wave | 代理 | 内容 | 验收 |
|---|---|---|---|
| W0 | INT | §4.3 契约冻结 + 权重校验（AI_Applications 目录可达性）+ 基线 commit | 基线 cargo+前端全绿 |
| W1-A1 | MOD-A1 | qwen3-tts 三模（clone/custom/synthesize-instruct）、9 音色、11 语言 | adapter 三能力函数可调用；decide 变体从权重 |
| W1-A2 | MOD-A2 | paddleocr doc_understand（全链路 + 开关） | 图片/PDF → 结构化 markdown；recognize 回归 |
| W1-A3 | MOD-A3 | faster-whisper precision + 8 变体声明 | precision 解析；变体切换后模型可用 |
| W1-A4 | MOD-A4 | qwen3-asr SRT 输出 + 30 语言枚举 + 1.7B 接线 | SRT 文件产物可下载；语言提示生效 |
| W1-A5 | MOD-A5 | rembg 4 变体 + alpha 降级语义 | 各变体可跑；不支持的 alpha 警告不 crash |
| W1-A6 | MOD-A6 | deep-filter 7 新参数 | 参数透传 CL；乱参报 400 |
| W1-A7 | MOD-A7 | firered-ocr recognize_pdf（PDF+DPI+多图） | PDF → md 文件；单图回归 |
| W2 | W8 | 权重导入 6+4+3+2+8（视 reachable 而定） | 每变体 models/ 目录 .ep_meta 位 + 变体 Ready |
| W3 | INT | 集成 + E2E 每模块 ✗1 真实样本 | 8 模块冒烟全过；cargo/front 全绿；PROGRESS |

---

## 7. E2E 冒烟清单（W3）

1. 快速调用页 → qwen3-tts clone_voice：参考音频 + 零样本克隆 → WAV 产物（26KB+）;
2. qwen3-tts custom_voice：spk_id=Vivian → WAV；
3. paddleocr doc_understand：真实 PDF/图 → 结构 markdown（表格/公式可识别）；
4. faster-whisper：precision=int8 + 中文样本 → 文本产物；
5. qwen3-asr：timestamps+srt_output → .srt 下载；语言提示中文强转；
6. rembg：u2netp 变体 → PNG；alpha_matting 降级 warn；
7. deep-filter：post_filter+pf_beta=0.1 → 降噪 WAV；
8. firered-ocr：PDF dpi=300 → md 文件下载；
9. 管线回归：video_to_srt 全流程不受影响；
10. 快速调用页/任务中心/模块页零控制台报错。

---

## 8. Backlog（本次不做）

- 未导入源应用的模块化：IOPaint / FaceFusion / Sam3 / SoulX / LatentSync / LTX / Wan / Comfy / A1111 / LLM-Translator（独立的「模型矩阵」计划）
- 流式 ASR/TTS（vLLM）|
- realesr scale 参数映射（源应用 scale 2-4 与 xsx2/4 对应，backlog）

---

## 9. 风险与缓解

| # | 风险 | 缓解 |
|---|---|---|
| R1 | qwen3-tts 1.7B 三模显存需求高（>8G 显存） | manifest vram 声明完整；W2 导入后实测；跑不动则备选 0.6B 三模接口退化提示 |
| R2 | paddleocr PP-StructureV3 子模型依赖 13 个、结构复杂 | A2 先做能力骨架 + 子模型按需求导入；recognize 保留兜底 |
| R3 | 源应用权重目录（HF snapshot/onnx）路径与 module 的 target_dir 命名不一致 | W0 权重校验表；导入时自定义 target_dir |
| R4 | frontend 无改动假设失效（例如新能力在前端品类） | INT 兜底把 api/types.ts + i18n 变更吸收 |
| R5 | 多代理并行改 venv 冲突（同一 runtime/venvs） | 每个模块 venv 独立——runtimes/venvs/<module_id> 天然隔离，无冲突 |

---

## 10. 待确认决策点

| # | 问题 | 推荐 |
|---|---|---|
| Q1 | qwen3-tts 三模（clone/custom/instruct）是否全做？ | ✅ 全做（源应用核心功能面） |
| Q2 | paddleocr 的 doc_understand 与 recognize 并存（不做纯替换）？ | ✅ 并存，recognize 保持兼容 |
| Q3 | 权重从 AI_Applications 本地导入（不走 HF 下载）？ | ✅ 走本地导入，最大限度省带宽 |
| Q4 | 8 个新变体（whisper）全部声明还是仅声明可本地导入的？ | ✅ 全部声明，缺权重时提示下载/导入 |
| Q5 | 流式能力（ASR/TTS vLLM）明确定位 backlog？ | ✅ backlog |
