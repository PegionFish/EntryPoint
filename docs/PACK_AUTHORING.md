# 整合包作者指南 (Pack Authoring Guide)

> 适用于 EntryPoint v0.x | 契约依据：PACK_UNIFY_PLAN §4（设计 B：模型整合包 SDK）

本文档面向**整合包作者**：如何把自己调好的"模型 + 管线 + 运行约束"打包成
`.epzip` 分发，以及他人如何导入使用。

---

## 1. 什么是整合包

整合包（pack）= **模型 + 管线 + 运行约束** 的可分发单元：

- 包是"应用"，管线是"配置"；
- 模型权重可以**随包携带**（bundle），也可以**仅引用**（reference，导入方按
  模块声明的下载源自行下载）；
- 分发渠道：GitHub Release / 任意 URL / 本地路径 / WebUI 浏览器上传；
- 导入后 EntryPoint 自动完成校验、落位、适配（见 §7 平台自动适配）。

**安全模型（重要）**：整合包**不携带可执行代码**。包内只有清单（TOML）、
校验和表、模型权重与管线定义；模块按 id 引用且必须在导入方机器上已安装
（缺模块会体现在适配报告里，而不是静默失败）。因此导入一个整合包等价于
"按清单摆文件 + 注册配置"，不会执行任何包作者提供的代码。

---

## 2. 归档布局（`.epzip`，冻结契约）

`.epzip` 就是一个 zip 归档，内部布局：

```
<pack-id>-<version>.epzip
├── ep-pack.toml              ← 必须。包清单（§3 字段参考）
├── CHECKSUMS.toml            ← 必须。包内所有文件的 sha256（§4）
├── models/<target_dir>/      ← 可选。bundle 模式的模型权重
└── pipelines/*.toml          ← 可选。管线定义（PIPELINE_SPEC.md 格式）
```

归档纪律（导入器强制执行）：

- 条目路径一律 `/` 分隔；禁止绝对路径、`..` 分段、反斜杠、保留名；
- 禁止符号链接条目与特殊文件条目（FIFO/设备等）；
- 条目名大小写冲突视为重复条目；
- 内容总大小受导入大小上限约束（复用上传约束）；
- 清单 `ep-pack.toml` 必须位于归档根部。

---

## 3. `ep-pack.toml` 全字段参考

serde 惯例对齐 module.toml：小写枚举、Option 字段可省略、缺省取默认值。

```toml
[pack]
id = "pigeonfish.subtitle-kit"     # <publisher>.<pack-name>，全局唯一键
version = "1.0.0"                  # semver（正式版本比较）
name = "字幕制作整合包"
description = "视频转字幕 + 降噪一体化"
authors = ["pigeonfish"]
license = "MIT"                    # 可选
homepage = "https://github.com/pigeonfish/subtitle-kit"   # 可选
min_ep_version = "0.1.0"           # 可选。要求导入方的最低 EntryPoint 版本（semver）
tags = ["字幕", "视频"]             # 可选。检索/圈选用

[compute]
backends = ["cuda", "cpu"]         # 包声明可利用的后端（导入时与本机设备比对）
notes = { rocm = "需 torch-rocm wheel" }   # 可选。每后端运行备注（自由文本，展示用）

[[models]]
qualified_id = "ep.systran.faster-whisper"   # 全限定模型 ID（§3.3）
variant = "large-v3"                          # 该模块声明的模型变体 id
mode = "reference"                            # reference | bundle（§5）
tags = ["字幕"]                               # 可选。导入后写入模型 meta，随包流转

[[pipelines]]
file = "pipelines/video_to_srt.toml"          # 归档内相对路径（/ 分隔，无 ..）
```

### 3.1 `[pack]` 字段

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `id` | string | ✅ | `<publisher>.<pack-name>`；两段各匹配 `^[a-z0-9][a-z0-9-]*$`；已安装包的唯一键 |
| `version` | string | ✅ | 语义化版本（semver），用于版本比较 |
| `name` | string | ✅ | 显示名称 |
| `description` | string | ✅ | 一句话描述 |
| `authors` | string[] | ❌ | 作者列表 |
| `license` | string | ❌ | 许可证标识（SPDX） |
| `homepage` | string | ❌ | 项目主页 / 发布页 URL |
| `min_ep_version` | string | ❌ | 最低 EntryPoint 版本（semver）；低于该版本导入被拒 |
| `tags` | string[] | ❌ | 包级标签 |

### 3.2 `[compute]` 字段

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `backends` | string[] | ✅ | 包可利用的计算后端（`cuda` / `rocm` / `openvino` / `directml` / `cpu`），导入时与本机设备比对生成适配报告 |
| `notes` | table | ❌ | 每后端的运行备注（自由文本，展示用；如 ROCm 需要额外 wheel） |

### 3.3 `[[models]]` 字段

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `qualified_id` | string | ✅ | 全限定模型 ID：`<publisher>.<vendor>.<model>`，各段 `^[a-z0-9][a-z0-9-]*$`；保留发布者 `ep`（仓库内置模块）。现有模块的简单 id 自动归一为 `ep.<vendor>.<model>` |
| `variant` | string | ✅ | 模块 `[[models]].id` 声明的变体名（如 `large-v3`）；与 qualified_id 组合为 `<qualified_id>@<variant>` pin |
| `mode` | enum | ✅ | `reference`（仅描述符）或 `bundle`（权重随包），见 §5 |
| `tags` | string[] | ❌ | 导入后写入模型 `.ep_meta.json` 的 tags |

> **模块依赖约束**：`qualified_id` 指向的模块必须在导入方机器上**已安装**
> （整合包不携带模块代码）。缺模块时导入不会静默失败——适配报告会逐条列出。

### 3.4 `[[pipelines]]` 字段

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `file` | string | ✅ | 归档内相对路径（`/` 分隔，禁止 `..` 与绝对路径）。管线文件格式见 PIPELINE_SPEC.md |

管线导入时做结构校验；与导入方已有管线重名时生成冲突报告（覆盖/改名由用户选择）。

---

## 4. `CHECKSUMS.toml`（完整性校验）

导入流程**先验后落盘**：解包到暂存目录后，先全量校验 CHECKSUMS，再落位任何文件。

```toml
[checksums]
"ep-pack.toml" = "8f3a…（sha256 小写 hex）"
"pipelines/video_to_srt.toml" = "c21b…"
"models/faster-whisper-large-v3/model.bin" = "0d77…"
```

规则：

- 条目 = 归档内相对路径（恒 `/` 分隔）→ 该文件内容的 **sha256 小写 hex**；
- `CHECKSUMS.toml` 自身不入表（无法自哈希）；
- 校验报告三类差异：**缺失**（表内有、磁盘无）、**多余**（磁盘有、表内无）、
  **篡改**（双方都有但哈希不符）——任何一类非空即导入失败；
- 用 `ep-pack build` 构建时自动生成，**不要手写**。

---

## 5. bundle vs reference（模型携带语义）

| 模式 | 含义 | 适用场景 |
|---|---|---|
| `reference` | 包内只有描述符（qualified_id@variant）；导入时按**模块 manifest 声明的下载源**后台下载（进度走 WS） | 权重体积大、或有公开下载源；分发包体积小 |
| `bundle` | 权重文件随包携带（归档 `models/<target_dir>/`）；导入时直接落位 | 私有/离线权重；保证导入即可用 |

导入语义（两种模式共同点）：

- 模型落位到 `models/<target_dir>/`，**绝不合并进已有目录**（目标目录已存在 →
  冲突报错）；
- 落位后写入 `.ep_meta.json`：`source = "pack"`、`pack_id`、`qualified_id`、
  `tags`（随包流转）；
- bundle 声明但归档缺权重文件 → 导入失败（`errorBundleMissing`）。

**"tag 组装"闭环**：在统一页给模型打 tag → 构建向导按 tag 一键圈选模型
（逐模型仍可选 bundle/reference）。

---

## 6. 打包与导入工具

### 6.1 `ep-pack-cli`（离线作者工具）

独立二进制，与 daemon 共用 ep-pack 库。命令面（Wave 3 C6 交付；具体参数签名
以 `ep-pack --help` 为准）：

| 命令 | 用途 |
|---|---|
| `ep-pack new` | 脚手架：生成包源目录（ep-pack.toml 模板 + pipelines/ 骨架） |
| `ep-pack validate` | 校验包源目录或 .epzip：清单 schema + 校验和 + 路径安全，不落盘任何东西 |
| `ep-pack build` | 从包源目录构建 .epzip（生成清单校验 + CHECKSUMS.toml + 打包） |
| `ep-pack import` | 命令行导入 .epzip 到指定 EntryPoint 根（走与 daemon 相同的 ep-pack 导入编排） |
| `ep-pack info` | 只读查看 .epzip 的清单摘要 / 内容清单 / 适配前置信息 |
| `ep-pack export` | 从已安装包注册表导出 .epzip（与 `GET /api/packs/{id}/export` 同源） |

典型作者工作流：

```bash
ep-pack new my-kit                  # 1. 脚手架
# 2. 编辑 ep-pack.toml（§3），放入 pipelines/*.toml 与（bundle 时）models/ 权重
ep-pack validate my-kit             # 3. 自检
ep-pack build my-kit -o my-kit-1.0.0.epzip   # 4. 出包（CHECKSUMS 自动生成）
# 5. 上传 GitHub Release（§8）
```

### 6.2 daemon HTTP API（集成方/WebUI 消费）

| 方法+路径 | 说明 |
|---|---|
| `GET /api/packs` | 已装包列表（注册表 `runtime/packs/<pack-id>.json`） |
| `POST /api/packs/import` | `{source:"local",path}` 或 `{source:"url",url}` → 202 `{pack_id}`，进度走 WS `pack_import` |
| `POST /api/packs/upload` | multipart `.epzip` 浏览器上传 → 202 同上 |
| `GET /api/packs/{id}` | 详情（内容清单 / 适配报告） |
| `DELETE /api/packs/{id}` | 卸载；`?keep_models=true` 保留模型文件 |
| `POST /api/packs/build` | `{models:[qualified_id@variant], pipelines:[id], bundle:[qualified_id], tags?:[tag]}` → 202，构建完成可下载 |
| `GET /api/packs/{id}/export` | `.epzip` 流式下载 |

---

## 7. 导入流程与平台自动适配

导入编排（`POST /api/packs/import` / `upload` 后台执行，进度经 WS `pack_import`）：

```
来源(local/url/upload) → 暂存 .pack-staging/<id>/
 → 解包（路径清洗 + symlink 逃逸防护）
 → CHECKSUMS.toml 全量校验（先验后落盘）
 → ep-pack.toml 校验（schema + min_ep_version + 模块存在性 + 后端适配）
 → models: bundle → 落位 models/<target_dir> + 写 meta(source=pack)
          reference → 按模块 manifest 解析下载源，后台下载（进度链复用）
 → pipelines: 校验后落 config/pipelines/（重名 → 冲突报告）
 → 注册 runtime/packs/<pack-id>.json（版本/内容清单/安装时间）
 → 清理冗余缓存副本
```

**适配报告**（导入前逐模型输出）：

- `将运行于 <device>` — 本机检测到匹配后端设备；
- `CPU 保底` — 未检测到匹配的加速设备，回退 CPU；
- `不支持：<原因>` — 模块缺失 / 后端不可用等（附原因）。

分层机制：

- **权重层**：onnx / safetensors / CTranslate2 格式天然跨平台，无需处理；
- **二进制层**：包内 native 资产用 `<os>-<arch>` key 表（如 `linux-x86_64` /
  `windows-x86_64`），导入器按当前平台选择；
- **依赖层**：模块 venv 本就按平台重建；`[compute].notes` 给出后端依赖提示。

---

## 8. 分发渠道

| 渠道 | 导入方式 |
|---|---|
| **GitHub Release**（推荐） | 把 `.epzip` 作为 Release 资产上传；导入方 `POST /api/packs/import {source:"url",url}` 填 Release 资产直链，或 `ep-pack import` |
| 任意 HTTP(S) URL | 同上（URL 导入） |
| 本地文件 | `{source:"local",path}` 或 `ep-pack import <file>` |
| 浏览器上传 | WebUI 整合包页（`POST /api/packs/upload`） |

> EntryPoint 本身**不托管**包索引/商店；发现与传播由作者自选渠道（Release
> 描述里附上 `ep-pack info` 输出与依赖模块列表是好习惯）。

---

## 9. 作者检查清单

- [ ] `pack.id` 形如 `<publisher>.<pack-name>`，两段均为小写字母数字连字符
- [ ] `version` / `min_ep_version`（若有）为合法 semver
- [ ] 每个 `[[models]].qualified_id` 指向的模块真实存在且导入方需要预装（在 README 里列明）
- [ ] bundle 模型的权重文件已放入 `models/<target_dir>/`，与清单声明一致
- [ ] `[[pipelines]].file` 均为包内相对路径（`/` 分隔、无 `..`）且管线文件能通过 PIPELINE_SPEC 校验
- [ ] `[compute].backends` 反映真实支持面，`notes` 写清额外依赖
- [ ] `ep-pack validate` 全绿后再 `build`
- [ ] 发布附言：依赖模块清单、权重许可、适配提示

---

## 10. 模块的标准压缩包分发（v1.3-draft 增补）

> 本节面向**模块作者**（分发 `modules/<id>/` 目录），与上文整合包（`.epzip`）是
> 两条独立通道。设计依据：HETERO_DIST_PLAN §2.2（分发载体）与 §2.3（平台导入/
> 导出 API）。

**不存在 `.epmod` 式自定义格式。** 历史上的 `.epmod` 提案已撤销（HETERO_DIST_PLAN
v2 变更）：模块分发一律采用**标准压缩档案**——`modules/<module-id>/` 目录本身就是
分发单元，打成 zip 或 tar.gz 即为发布物；任何"根部含一个 `module.toml`"的标准
压缩包都可被平台导入。不引入专有扩展名、专有清单或专用工具链。
（`.epzip` 整合包为已交付的历史功能，维持现状、不扩散，见 §1–§9。）

发布物清单：

```
<module-id>-<version>.zip        ← 内容即 modules/<module-id>/ 目录本身
<module-id>-<version>.tar.gz     ← 同内容第二格式，按平台习惯二选一亦可
SHA256SUMS.txt                   ← 全部发布物的 sha256 清单（sha256sum -c 可验）
```

- 包内布局 = MODULE_SPEC §1 模块目录结构，`module.toml` 位于包根（或唯一一级目录下）；
- 完整性交给用户侧工具链：发布侧出 SHA256SUMS.txt，用户自行校验；
- 权重一律不随包（HETERO_DIST_PLAN §2.4 三级策略）；Tier B/C 模型在 module.toml
  的 `[distribution]` 中声明 `license_note` / `guide_url`（MODULE_SPEC §2.2）。

用户获取路径（三选一，HETERO_DIST_PLAN §2.1）：

| 路径 | 说明 |
|---|---|
| a. WebUI 导入页上传 | 上传压缩包 → 服务端安全解包校验后落位 |
| b. 下载 + URL/上传导入 | 自行从 GitHub Releases 等渠道下载压缩包，经 WebUI/API 上传或 `import-url` 直链导入 |
| c. 完全手动解压 | 解压到 `modules/<id>/` → 刷新识别（poweruser 首选，一等公民路径） |

托管建议：GitHub Release 挂上述三个文件即可；EntryPoint 不运营商店/注册表，
发现与传播由作者自选渠道负责。
