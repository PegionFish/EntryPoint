# EntryPoint 桌面端退役（Sunset）设计方案

- **状态**: 待执行（用户 2026-08-13 裁决）
- **决策链**: 第三轮双端 E2E（`reports/e2e_uiux_report_20260813.md`）→ WebUI 设计完成度与可用性显著领先 → 用户裁决「砍掉桌面端，WebUI 为唯一 UI，server 形态交付」
- **替代方案留档**: WebView 薄壳内嵌方案（wry + 进程内嵌 axum）曾作为候选，随「砍掉桌面端」裁决一并放弃，见 §8

---

## 0. 背景与决策依据

第三轮 E2E 实测（真实显示器 + CDP 双通道）结论：

| 维度 | WebUI | 桌面端 |
|---|---|---|
| 五页 + 核心流程 | 全部通过，零控制台错误 | 受自动化限制部分验证 |
| 阻断缺陷 | 无 P0/P1 | **D1 统计卡拉伸致主内容不可达（P1）** + D2 浅色对比度（P2） |
| 历史 P0 | — | 最大化冻结/浅色白屏虽已修复，呈现层稳定性记录不良 |
| 维护成本 | 单一 React 代码库 | 9000 行 egui + 3451 行 main.rs 重复实现 daemon 已有逻辑 |

桌面端 `main.rs` 的 `background_loop` 在进程内重复实现了 ep-daemon 已具备的全部能力（模块生命周期/直跑/任务注册表/整合包导入导出/更新检查），且 UI 层永远追不平 WebUI（egui 无 backdrop-blur/渐变画布/CSS 动画，React Flow 编辑器无法对等复刻）。继续双端维护 = 持续漂移（本轮 W1-W14 vs D1-D4 即漂移证据）。

**裁决**：退役 ep-desktop，WebUI 为唯一 UI，产品交付 = ep-daemon + WebUI 静态资源（server 包）。

---

## 1. 目标架构

```
交付物（Windows: build.ps1 server / Linux: build.sh server）
├── bin/ep-daemon.exe + ep-pack.exe + VC 运行库
├── webui/            ← React 前端产物（唯一 UI）
├── config/  modules/  workspace/
└── start-daemon.bat  ← 用户双击 → 浏览器访问 http://127.0.0.1:9800

Linux 服务器部署：systemd 服务 + 远程浏览器管控（现状不变）
```

workspace 由 6 crate 收敛为 **5 crate**：`ep-core / ep-daemon / ep-webui / ep-pack / ep-pack-cli`。

---

## 2. 功能清点与处置

### 2.1 需移植（桌面独有、daemon 缺失）

| 功能 | 桌面位置 | 处置 | 目标 |
|---|---|---|---|
| **Windows 子进程错误弹窗抑制**（`SetErrorMode(SEM_FAILCRITICALERRORS\|NOGPFAULTERRORBOX\|NOOPENFILEERRORBOX)`，防 python/uv DLL 初始化失败弹系统对话框） | ep-desktop/src/main.rs:20-50 | **移植** | ep-daemon/src/main.rs `main()` 最早期（先于任何子进程拉起）+ `--run-module` 模式同覆盖；补 cfg(windows) 单测断言函数存在性 |

### 2.2 直接丢弃（daemon/WebUI 已对等覆盖，本轮 E2E 验证）

模块启停/健康检查/日志、直跑（含 wait/callback）、管线 CRUD+执行、任务中心+产物下载、模型下载/上传/导入/更新、整合包导入导出、配置持久化、i18n、主题——WebUI 全链路实测通过，零缺口。

### 2.3 随退役消失的桌面独有能力（已裁决接受）

双击即用 exe、系统托盘、窗口状态持久化、egui 原生渲染。换得：单一 UI 代码库、零漂移、少维护一个 9000 行 crate。Windows 用户体验由「双击 bat + 浏览器」承担（§3 C2 优化启动体验）。

### 2.4 死代码/死配置清理

| 项 | 位置 | 处置 |
|---|---|---|
| `UiConfig` 结构体 + `AppConfig.ui` 字段 | ep-core/src/config.rs:339,442 | 删除（已核实无 `deny_unknown_fields`，旧配置含 `[ui]` 节可安全解析忽略；**补回归测试**：含 `[ui]` 节的 TOML 反序列化不报错） |
| `desktopPages` / `desktopApp` i18n 命名空间 | ep-core/src/i18n.rs:43（NAMESPACES）+ 对应 locale JSON | 删除命名空间注册 + locale 文件（先 grep 确认无 daemon 消费） |
| config/app.toml `[ui]` 节 | config/app.toml + docs/CONFIG_REFERENCE.md | 删除节 + 文档章节 |
| ep-core 中仅桌面消费的 pub 项 | 全 crate | Wave 0 审计产出清单，Wave 2 清理（clippy dead_code 辅助） |

---

## 3. 删除与修改清单（文件级）

| # | 目标 | 操作 |
|---|---|---|
| 1 | `crates/ep-desktop/`（整目录，~9000 行） | 删除 |
| 2 | `Cargo.toml` workspace members | 移除 `"crates/ep-desktop"`；`Cargo.lock` 重新生成 |
| 3 | `build.ps1` | 移除 `gui` 模式：`ValidateSet` 收敛为 `server`；调用 `gui` 时打印迁移提示退出（不静默）；删除 gui 分支（launcher/清单逻辑） |
| 4 | `build.sh` | 移除 `gui` 模式与 macOS `.app` 分支（CRATE=ep-desktop 段:228 起）；`server` 为唯一模式 |
| 5 | `packaging/PKGBUILD.gui` + `packaging/entrypoint.desktop` | 删除；核查 `PKGBUILD`（server）与 `entrypoint.install` 无 gui 残留引用 |
| 6 | `config/app.toml` `[ui]` 节 + `docs/CONFIG_REFERENCE.md` 对应章节 | 删除 |
| 7 | ep-core `UiConfig`/`ui` 字段 + i18n desktopPages/desktopApp | 删除 + 回归测试 |
| 8 | `README.md` | 删除「桌面端构建/Arch GUI/桌面功能」段落；改为 server-only 快速开始（双击 bat → 浏览器）；状态段更新 |
| 9 | `DESIGN.md` 架构图（:1039 ep-desktop 节点） | 更新为 5-crate 架构 |
| 10 | `PROGRESS.md` | 追加「桌面端退役」章节（裁决/范围/commit 链） |
| 11 | 历史文档（NIGHTLY_PLAN / DESKTOP_BACKPORT_PLAN / UNIFIED_UI_REDESIGN_PROPOSAL / WEBUI_DEV_PLAN / PIPELINE_SPEC） | **不改写**，文首加 sunset 横幅（「本文档所述 ep-desktop 已于 2026-08-13 退役，见 DESKTOP_SUNSET_PLAN.md」） |
| 12 | `reports/` 桌面评估类报告 | 保留（历史证据），不动 |

---

## 4. 多代理并行执行计划

> 编排者 = 主会话；执行代理 = 能力上限全开的子代理（允许嵌套开孙代理做内部并行，但写权限不超出本代理所有权）。峰值并行 **8**。

### Wave 0 — 审计（2 代理并行，只读）

| Agent | 范围 | 交付物 |
|---|---|---|
| **A1 GapAudit** | 逐函数 diff `ep-desktop/src/main.rs background_loop` 与 ep-daemon API + WebUI 覆盖 | 缺口报告：确认「2.2 丢弃清单」零遗漏；任何 daemon/WebUI 未覆盖的桌面能力 → 升级为移植项 |
| **A2 RefAudit** | 全仓库 `ep-desktop|entrypoint\.desktop|PKGBUILD\.gui|\[ui\]|desktopPages|desktopApp|UiConfig` 引用扫描 | 删除清单 v2（文件:行级，覆盖 §3 并补漏）+ ep-core 死代码候选清单 |

**Wave 0 门禁**：两份报告经编排者确认 → 冻结 §3 清单为执行基线（契约先行）。

### Wave 1 — 移植与体验（3 代理并行）

| Agent | 独占文件 | 职责 |
|---|---|---|
| **B1 ErrorDialogPort** | ep-daemon/src/main.rs | SetErrorMode 移植（server + run-module 双入口最早期）+ cfg(windows) 测试 |
| **B2 LauncherUX** | start-daemon.bat（build.ps1 生成段）、README 快速开始素材 | Windows 启动体验：bat 启动后自动 `start http://127.0.0.1:9800` 开默认浏览器 + 控制台提示；可选 `--no-browser` 参数 |
| **B3 ConfigTolerance** | ep-core/src/config.rs + 测试 | 先补「含 `[ui]` 节旧配置可解析」回归测试（锁死 serde 容忍语义），为 Wave 2 C4 删字段铺路 |

### Wave 2 — 退役删除（8 代理并行，峰值 8）

| Agent | 独占文件 | 职责 |
|---|---|---|
| **C1 CrateRemoval** | `crates/ep-desktop/`（删）、`Cargo.toml`、`Cargo.lock` | 删 crate + workspace member + lock 重生成；`cargo check --workspace` 自验 |
| **C2 BuildPs1** | `build.ps1` | gui 模式移除 + 迁移提示；server 流程回归自验 |
| **C3 BuildSh** | `build.sh` | gui/macOS-app 分支移除；server 流程语法自验（bash -n）+ Linux 侧说明 |
| **C4 Packaging** | `packaging/*` | 删 PKGBUILD.gui + entrypoint.desktop；PKGBUILD/install 去 gui 残留 |
| **C5 ConfigDocs** | `config/app.toml`、`docs/CONFIG_REFERENCE.md` | 删 `[ui]` 节 + 文档章节 |
| **C6 CoreDeadCode** | `ep-core/src/config.rs`（UiConfig）、`ep-core/src/i18n.rs` + locale JSON | 删 UiConfig/ui 字段 + desktopPages/desktopApp 命名空间与 locale 文件（依 A2 清单）；ep-core 测试自验 |
| **C7 DocsCore** | `README.md`、`DESIGN.md`、`PROGRESS.md` | server-only 快速开始 + 5-crate 架构图 + 退役章节 |
| **C8 DocsBanner** | 5 份历史文档（§3-11） | 文首 sunset 横幅（可嵌套孙代理按文件并行，写权限限本清单） |

### Wave 3 — 门禁与验收（2 代理串行）

| Agent | 职责 |
|---|---|
| **D1 GateRunner** | Windows 门禁：`cargo clippy --workspace --all-targets` + `cargo test --workspace` + 前端 `npm run build` + `build.ps1 server` 全量 + 产物清单核验（无 entrypoint.exe/gui 残留）；`build.ps1 gui` 调用验证迁移提示 |
| **D2 E2EVerify** | 实机验收：server 包解压运行 → WebUI 全页面冒烟（复用本轮 `runtime/e2e-r3` 矩阵：仪表盘/模块启停/直跑/管线/任务/设置/主题/响应式）+ API 40 项 + 控制台零错误；Windows 弹窗抑制抽查（故意触发缺失 venv 探测无系统弹窗） |

---

## 5. 并行开发规则（PACK_UNIFY_PLAN §9 强化版）

1. **文件独占**：Wave 内所有权矩阵排他写；越界写禁止 → 交付物列「仲裁请求」由编排者执行。只读不限。
2. **隔离工作树**：每代理 `isolation: "worktree"`；编排者门禁后统一合并，代理不自行 merge/rebase。
3. **契约先行**：Wave 0 两份报告 = 冻结基线；清单修订须编排者确认并广播。
4. **提交纪律**：每代理完成即在 worktree 分支 commit（防丢失），commit 信息带 agent 标识（`chore(sunset/C1): ...`）。
5. **删除安全**：只允许删 A2 清单内路径；`git rm` 前核对清单；清单外文件一律不动（保护用户工作区未提交改动，尤其 `config/app.toml` 本地修改——C5 只删 `[ui]` 节，保留其余本地值）。
6. **波次门禁**：波末编排者统一跑 §6 门禁；全绿开下一波；失败定位到责任代理返工（返工不跨波并行）。
7. **同树容忍**：合并窗口内非所有权文件的编译错误忽略不代修，记录交门禁仲裁。
8. **验证内建**：每代理返回前跑通自己范围检查（C1: cargo check；C6: cargo test -p ep-core；C2/C3: 脚本自验；C7/C8: 链接/引用 grep 复核）并附清单。
9. **能力上限条款**：代理可嵌套 spawn 孙代理做内部并行（如 C8 按文件扇出、A2 按目录扇出扫描），但孙代理写权限继承父所有权；嵌套深度 ≤2；编排者只对接顶层代理交付物。
10. **双平台纪律**（沿用 2026-08-04 裁决）：一切改动 Windows + Linux 同时可用；Linux 编译面靠 cfg 纪律（workspace 交叉 check 被 openssl-sys 阻塞的既有约束不变）。

---

## 6. 门禁与验收标准

### 门禁命令（Windows 执行机）
```
cargo clippy --workspace --all-targets   # 零警告
cargo test --workspace                    # 全过（数量随删除略降，以实际为准）
cd crates/ep-webui/frontend && npm run build
.\build.ps1 server                        # 全量流程绿
```

### 验收标准
- [ ] workspace = 5 crate；`cargo tree` 无 egui/eframe/accesskit 依赖残留
- [ ] `crates/ep-desktop/` 不存在；全仓库活跃代码/构建/打包零 `ep-desktop` 引用（历史文档仅横幅）
- [ ] `build.ps1 gui` → 打印迁移提示并以非零码退出；`build.ps1 server` 产物含 ep-daemon + webui + start-daemon.bat，**无** entrypoint.exe
- [ ] ep-daemon 含 SetErrorMode 抑制（Windows 实机：缺失 venv 探测不弹系统错误框）
- [ ] 含 `[ui]` 节的旧 config/app.toml 可被新 daemon 正常加载（容忍回归测试在案）
- [ ] WebUI 实机冒烟全过（本轮矩阵复跑）+ 控制台零错误
- [ ] README 快速开始 = server-only 路径，新用户 3 步内跑通

### 回滚
每波合并为独立 commit（`chore(sunset/Wn): ...`）；任一验收失败 → `git revert` 对应波次 commit 链，workspace 恢复 6-crate 现状，不影响已发布 server 包。

---

## 7. 风险

| 风险 | 缓解 |
|---|---|
| ep-core 存在未识别的桌面独占 pub API，删除后 daemon 编译断裂 | A2 死代码清单 + C6 以 `cargo check --workspace` 为尺，断裂项回退保留并标注 |
| 旧用户配置含 `[ui]` 节导致解析失败 | B3 先行锁死容忍测试；serde 默认忽略未知字段已核实 |
| 历史文档引用造成新人困惑 | 横幅 + README 单一权威入口 |
| Windows 用户失去双击 exe 体验 | B2 自动开浏览器 + README 图示；可后续评估「bat 转 exe 壳」（非本计划范围） |
| 用户工作区 `config/app.toml` 本地改动被误伤 | 规则 5：C5 仅删 `[ui]` 节，diff 审查后提交 |

---

## 8. 附录：已否决的替代方案

**WebView 薄壳方案**（wry/WebView2 渲染现有 React WebUI + 进程内嵌 axum router）：可实现 100% UI 对等与双击即用，壳仅 ~500 行。否决原因：用户裁决产品定位为 server 优先，桌面壳的边际价值（托盘/自启/窗口身份）不足以支撑额外交付形态；且保留壳仍需维护 Windows 打包/WebView2 依赖/单实例等外围成本。若未来需要 Windows 开箱体验，可基于本方案退役后的干净基线重启该选项（ep-daemon router 已具备 `build_app_router(state, static_dir)` 可内嵌入口，见 ep-daemon/src/main.rs）。

---

*方案编制: 2026-08-13 · 依据 reports/e2e_uiux_report_20260813.md 实测证据 · 执行范式对齐 docs/PACK_UNIFY_PLAN.md §9-10*
