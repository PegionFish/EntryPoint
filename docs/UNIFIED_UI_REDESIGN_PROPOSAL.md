# EntryPoint 统一 UI 重设计方案（WebUI + 桌面端）

> ⚠️ **Sunset 横幅（2026-08-13）**：本文档所述 **ep-desktop 桌面端已于 2026-08-13 退役**，WebUI 为唯一 UI（server 形态交付）。本页保留为历史记录，不再维护；详见 [DESKTOP_SUNSET_PLAN.md](DESKTOP_SUNSET_PLAN.md)。

- 状态：提案（待裁决）
- 范围：只改设计/文档口径；实施须按第 11 节波次另行立项
- 编制：设计调研代理 · 2026-08-09

## 0. 背景与输入

EntryPoint 桌面端（egui/eframe）是 WebUI 的反向移植，此前仅做令牌级最小同步，页面级信息架构与交互打磨缺失。裁决口径升级：不是桌面端单向对齐 WebUI 现状，而是先建立统一设计语言，再对两端前端彻底重构。

输入清单（均已实测核对）：

- 用户实拍三页截图（模块 / 管线 / 设置）
- WebUI 前端源码 crates/ep-webui/frontend/src（pages + shared/ui 组件 + index.css）
- 桌面端源码 crates/ep-desktop/src（app.rs、pages/*、ui/*、theme.rs）
- docs/DESIGN_SYSTEM.md（令牌权威；注意其第 7 节「页面为占位」描述已过时，须随本方案升版）
- reports/desktop_ui_eval_20260807.md（实机评估，缺陷编号 P0-1…P3-1 沿用）
- 实测基线：egui/eframe/epaint 0.31.1；ep-desktop 单测 76 个；i18n 14 命名空间 x 2 语言

## 1. 美学设计：深空仪表盘（Deep-Space Console）

定位：本章是方案的灵魂，置于统一设计语言（§3）之前；§3 与 §6-§7 各实现章节的令牌、组件行为与动效参数均由本章六条美学主张推导。美学基准概念图：C:\Users\PegionFish\.qoder-cn\vibe_images\entrypoint-unified-ui-concept_1786206794.png，全部视觉验收以该图为准。

### 1.1 六条美学主张（主张 → 令牌 → 组件行为 → 动效参数 → egui 可行性）

**主张 1：深空底色，不用纯黑。**
- 令牌：三级层深——层 0 #0B0F17（应用背景）、层 1 #0E1420（卡片）、层 2 #121A2B（浮层/抬升面）；WebUI --background/--card/--popover 重定值，egui Palette bg_base/bg_card/bg_raised。
- 组件行为：用底色差表达空间层级而非边框；边框仅保留 1px 细描边；页面底=层 0，卡片=层 1，Drawer/Dialog=层 2。
- 动效参数：无（静态层级）。egui：完全可实现（Color32 常量）。

**主张 2：科技感点缀色克制——电光青→靛蓝渐变（#38BDF8 → #6366F1）只用于活跃态。**
- 令牌：--accent-gradient-from #38BDF8、--accent-gradient-to #6366F1、--primary #38BDF8；egui accent_from/accent_to/primary。
- 组件行为：仅四类活跃载体——运行光点/选中导航项/主按钮/进度微光；静止界面一律低饱和冷静。
- 动效参数：活跃态切换 150-200ms ease-out。egui：无渐变画刷→主按钮降级为单色 #38BDF8+Shadow 辉光；导航竖条与进度条渐变用双色分段插值近似。

**主张 3：卡片玻璃拟态 + 1px 内发光描边。**
- 令牌：--surface-glass（半透明 slate）、--border rgba(148,163,184,0.12)、--border-glow rgba(56,189,248,0.18)、--border-glow-strong rgba(56,189,248,0.45)；egui border/border_glow/border_glow_strong/surface_glass。
- 组件行为：hover 只提升描边亮度（border-glow → border-glow-strong），零位移；运行态卡片附加极淡呼吸辉光。
- 动效参数：hover 过渡 150ms；呼吸辉光 2.4s ease-in-out 周期、不透明度 0.35-0.7。egui：无 backdrop-blur→半透明底+亮描边降级；呼吸辉光用周期性描边亮度插值近似（可实现）。

**主张 4：数据仪表盘化。**
- 令牌：大号等宽数字（--font-mono、tabular figures）、全大写灰阶小标签（11px、letter-spacing 0.08em、muted-foreground）、画布点阵 --grid-dot rgba(148,163,184,0.12)；egui mono 字体+grid_dot。
- 组件行为：StatStrip 大号等宽数字 + 全大写小标签；管线画布点阵底；设备卡与 VRAM 条数字一律 tabular 对齐。
- 动效参数：无。egui：完全可实现（mono 字体、大写标签、draw_grid 改点阵样式）。

**主张 5：状态色语义全局统一——就绪=冷绿、运行=青、缺失/错误=珊瑚红、停止=中性灰。**
- 令牌：--status-ready #4ADE80、--status-running #22D3EE、--status-error #FB7185、--status-stopped #94A3B8；egui service_status 映射同步重定值。
- 注：此四态按概念图重定值，覆盖 index.css 既有对应色值；dtype/cat 扩展色保持不变。
- 组件行为：StatusBadge、节点状态点、进度条、toast 全部统一四态色；运行态附青色辉光。egui：完全可实现（Color32）。

**主张 6：动效纪律。**
- 令牌：--duration-fast 150ms、--duration-base 200ms、--ease-standard cubic-bezier(0.4,0,0.2,1)；egui ANIM_MS 167。
- 组件行为：禁止弹跳与闪烁曲线；hover/展开/页面切换一律 150-200ms；管线连线 2px 渐变描边 + 数据流微光动画。
- 动效参数：连线流光 1.6s linear 循环；呼吸辉光 2.4s。egui：无 CSS 动画→时间驱动插值；数据流用虚线偏移或贝塞尔曲线上移动光点近似；动画重绘须与 REPAINT_WATCHDOG 联验防丢帧。

### 1.2 精确色值表（对应概念图，权威色源）

| 用途 | Hex | WebUI CSS 变量 | egui Palette 字段 |
|---|---|---|---|
| 应用背景（层 0） | #0B0F17 | --background | bg_base |
| 层深 1（卡片） | #0E1420 | --card | bg_card |
| 层深 2（浮层/抬升） | #121A2B | --popover / --surface-glass 基色 | bg_raised |
| 常规描边 | rgba(148,163,184,0.12) | --border | border |
| 内发光描边（静态） | rgba(56,189,248,0.18) | --border-glow | border_glow |
| 内发光描边（hover） | rgba(56,189,248,0.45) | --border-glow-strong | border_glow_strong |
| 强调渐变起（电光青） | #38BDF8 | --accent-gradient-from / --primary | accent_from / primary |
| 强调渐变止（靛蓝） | #6366F1 | --accent-gradient-to | accent_to |
| 就绪（冷绿） | #4ADE80 | --status-ready | status_ready |
| 运行（青） | #22D3EE | --status-running | status_running |
| 缺失/错误（珊瑚红） | #FB7185 | --status-error | status_error |
| 停止（中性灰） | #94A3B8 | --status-stopped | status_stopped |
| 主文本 | #E2E8F0 | --foreground | text_primary |
| 次级文本 | #94A3B8 | --muted-foreground | text_muted |
| 画布点阵 | rgba(148,163,184,0.12) | --grid-dot | grid_dot |

本表 hex 为方案权威色值；§3.1 的 hsl 令牌由本表换算并在 W0 落地，冲突时以本表为准。

## 2. 总体原则

1. **唯一基准**：新「统一设计语言」（第 2 节）是两端共同基准；两端互相不对齐现状，只对齐新规范。
2. **令牌先行**：先落令牌与基础组件视觉，再做页面；任何页面改动禁止引入规范外硬编码色值。
3. **IA 一致、交互分级**：信息架构两端完全一致；交互按 egui 能力降级，但状态/反馈/空态/错误态语义必须一致。
4. **深色为主、科技感克制**：霓虹蓝主色 + 青紫渐变只作小面积点缀；发光只表达「活跃/焦点/可交互」，静态元素不发光。
5. **可验收**：每波次以单测 + 截图目检清单 + 全量回归三重验证。
6. **不越界**：见第 9 节不做清单。

---

## 3. 统一设计语言（跨端视觉规范）

> 本章为 §1 美学主张的落地规范；权威色值以 §1.2 精确色值表为准。

> 本章是全方案的最高约束。WebUI 与桌面端的一切视觉决策都以本章为准。
> 风格定位：**现代简洁、深色为主、科技感点缀**。科技感通过「霓虹蓝主色 + 青紫渐变点缀 + 玻璃拟态卡片 + 精细描边 + 发光态」表达，但一律克制——只在需要吸引注意的位置使用。

### 3.1 色彩系统

现状：DESIGN_SYSTEM.md 第 1 节 已定义基础语义令牌（background/card/primary/…）与 status/dtype/cat 扩展色，两端大体落地（WebUI 在 index.css，桌面在 ui/palette.rs）。**保留全部既有语义色不改变色值**，新增以下科技感令牌：

| 令牌 | 深色值 | 语义与用途 |
| --- | --- | --- |
| --primary-glow | hsl(217 91% 60% / 0.35) | 主操作按钮/焦点环的外发光（box-shadow / egui Shadow） |
| --accent-gradient-from | hsl(199 89% 60%) | 青端：渐变起点（强调条、激活导航底边、进度条） |
| --accent-gradient-to | hsl(262 83% 66%) | 紫端：渐变终点；与 from 组成品牌渐变 |
| --surface-glass | hsl(240 6% 12% / 0.72) | 玻璃拟态浮层（抽屉/对话框/顶栏）底色 |
| --ring-glow | hsl(217 91% 60% / 0.5) | 输入件聚焦时的发光环（替代生硬 3px ring） |
| --status-glow-running | hsl(142 71% 45% / 0.4) | 运行中状态点的辉光 |

使用规则：
1. **渐变只允许 6 处**：侧栏激活项左侧 2px 竖条、Logo 字标、主按钮 hover 高光层、任务进度条、管线画布选中边、仪表盘统计大数字下划线。其余位置一律单色。
2. **发光三档**：弱（1px 描边带 8% 主色）用于卡片 hover；中（6-8px 模糊、透明度 35%）用于主按钮/聚焦；强（12px+）仅用于运行中状态点与管线执行中节点。
3. **玻璃拟态限定浮层**：Drawer/Dialog/CommandBar 用 surface-glass + backdrop-blur（WebUI）；桌面端无背景模糊能力，降级为「半透明底色 + 1px 亮描边 + 阴影」，见 §3.6 降级矩阵。
4. 浅色主题保留但降级为一等公民之外的目标：发光/渐变在浅色下自动关闭（仅留描边强调）。

### 3.2 字体排印

沿用 DESIGN_SYSTEM 第 2 节 五级层级，不新增字号；补充三条规则：

- 数值一律等宽（mono）：端口、VRAM、耗时、进度百分比、统计大数字。
- 大数字规格 text-4xl mono bold + 下方 2px 渐变下划线（仪表盘/任务页统计条）。
- 路径/ID/日志用 mono，且允许 break-all 换行，禁止被截断不可见。

### 3.3 圆角 / 间距 / 描边 / 阴影（发光）

| 项 | 规范 | 两端落地 |
| --- | --- | --- |
| 卡片圆角 | 10px（--radius-lg = 0.625rem） | WebUI rounded-lg；桌面 CARD_ROUNDING 由 12 改 10 |
| 控件圆角 | 8px | WebUI rounded-md；桌面 CONTROL_ROUNDING=8 已符合 |
| 徽章 | 全圆（999px） | 两端已符合 |
| 间距基准 | 4px 网格；页内边距 24px；卡片内边距 24px；区块间距 24-32px | WebUI p-6/space-y-6~8；桌面 card_frame inner_margin 由 16 改 24，页区统一 24 |
| 描边 | 1px，border 令牌；hover 时 border-primary/40 | 两端 Frame.stroke / border 类 |
| 普通阴影 | 浮层（popover/dialog）用 8-16px 模糊黑色 40% | WebUI shadow-lg；桌面 egui Shadow |
| 发光阴影 | primary-glow/status-glow 系列，见 §3.1 | WebUI box-shadow；桌面 epaint Shadow（color 支持彩色，实测 0.31.1 可用） |

### 3.4 组件视觉风格

**卡片**：card 底 + 1px border + 10px 圆角；hover 态 = border-primary/40 + 弱发光；「选中变体投影」等强调区块用 primary/5 底 + primary/25 描边（WebUI 现有样式保留为规范）。
**按钮层级（两端统一五档）**：
- primary（主操作，每屏至多 1 个）：primary 填充 + 中档发光；hover 提亮。
- secondary：muted 底；用于「运行/直跑」等次级入口。
- outline：透明底 + 描边；用于停止/保存等。
- ghost：无底无边框；用于日志/详情/刷新等高频低危动作。
- destructive：红填充或红色 ghost（按危险程度），删除/卸载一律先 ConfirmDialog。
**徽章**：胶囊 + 状态圆点 + 语义色 15% 底/30% 描边；过渡态（starting/preparing/running 进度）圆点脉冲，运行中附 status-glow-running 辉光。

**表格**：表头 12px muted 不加底色；行 hover = muted/50；数值列右对齐 mono；状态列用徽章；列宽按内容加权铺满（桌面既有规范，WebUI 对齐过来）。
**输入件**：input 底 + 1px border；聚焦 = ring-glow 发光环（两端统一替代硬 ring）；数值件（端口/间隔）必须呈现「可编辑外观」（带底带框，桌面 DragValue 加边框与提示文案）。

**进度条**：4-6px 高、圆角；填充用青→紫渐变（WebUI）或主色单色+端点亮点（桌面降级）；queued 态整体脉冲。

### 3.5 令牌双端映射（权威对照表）

| 令牌 | WebUI（index.css/Tailwind） | 桌面（ep-desktop） |
| --- | --- | --- |
| background/card/foreground | :root 与 .dark 的 CSS 变量 + @theme inline | Palette.bg/card/text（palette.rs 已映射） |
| primary/destructive/status-* | --primary 等 + text-status-* 工具类 | Palette.primary/danger/success/warning/info/neutral/notready |
| 新增 primary-glow/ring-glow/status-glow-running | CSS 变量 + .glow-* 工具类 | Palette 新增 glow 字段（Color32 带 alpha），Frame.shadow 彩色化 |
| accent-gradient-from/to | CSS 变量 + .bg-gradient-accent 工具类 | 不支持渐变画刷（0.31.1 实测无 Brush/Gradient）：降级单色或预生成 1px 纹理拉伸 |
| surface-glass | CSS 变量 + .glass 工具类（backdrop-blur） | 无背景模糊：降级半透明 card_raised + 亮描边 |
| 圆角/间距 | --radius 与 Tailwind 间距 | CARD_ROUNDING/CONTROL_ROUNDING 常量 + theme.rs spacing |
| 字号层级 | text-xl/base/sm/xs + text-4xl | theme.rs TextStyle 映射（Heading 20/Body 14/Small 11/Mono 13） |

### 3.6 egui 0.31.1 能力与降级矩阵（实测）

| WebUI 交互 | egui 0.31.1 支持情况 | 桌面策略 |
| --- | --- | --- |
| Drawer/Sheet 侧滑 | 无原生组件 | 自绘：右侧 SidePanel/Window + 动画 offset（150ms 缓动） |
| 背景模糊玻璃拟态 | 不支持 backdrop-blur | 半透明底 + 亮描边 + 彩色阴影 |
| 渐变填充 | epaint 无 Brush/Gradient（源码实测） | 单色 + 发光，或 1px 预生成渐变纹理拉伸 |
| CSS 脉冲/走针动画 | 无 CSS 动画 | request_repaint_after 驱动动画时钟；仅过渡态激活，空闲回到 2s 看门狗心跳 |
| ReactFlow 画布（拖拽/缩放/连线/Minimap/Controls） | 无图编辑库 | 复用桌面既有自绘能力（pipeline_editor.rs 已有 pan/zoom/连线/贝塞尔预览），按 §7.3 映射表逐项对齐 |
| HTML5 拖拽（节点库→画布） | egui::DragAndDrop（0.31.1 实测存在） | 用 DragAndDrop payload 替代 dataTransfer |
| 键盘滚动 | ScrollArea 默认仅滚轮 | 已有 keyboard_scroll 封装（components.rs，带单测） |

---

## 4. 设计理由与备选方案

本章为 §3 统一设计语言的论证补充：每个核心设计决策给出「选定方案 / 被否决备选 / 否决理由 / 验证方式」，防止重构期审美漂移，也为评审提供可追溯依据。

### 4.1 色彩决策

选定：蓝灰系深空底色（#0B0F17→#0E1420→#121A2B 三级层深）+ 电光青→靛蓝点缀渐变（#38BDF8→#6366F1，仅活跃态）+ 四态语义色（冷绿/青/珊瑚红/中性灰）。

备选与否决理由：
- 备选 A 纯黑底（#000）：OLED 观感锐利，但在主流 LCD 上发灰、失去层深；边框与辉光对比失衡，否决。
- 备选 B 高饱和多色霓虹（多主题色并存）：视觉噪声大，弱化信息优先级，且与「克制点缀」冲突，否决。
- 备选 C 紫粉赛博（synthwave）：与既定青-靛体系冲突，浅色模式可读性差，否决。
验证：对照概念图截图取色比对；WCAG 对比度校验（正文 ≥4.5:1，大号数字/图标放宽至 3:1）；四态色在色觉模拟（deuteranopia）下仍可区分。

### 4.2 布局与 IA 决策

选定：五页导航 IA（仪表盘/模块/管线/任务/设置）；管线「库优先两态」；模块「卡片→网格→抽屉/对话框」三层递进；设置页 WebUI 单列 max-w-4xl、桌面双列卡片。
备选与否决理由：
- 备选 A 单页工作台（全功能聚合）：导航成本低但信息过载，五页数据域本就分离，否决。
- 备选 B 顶部 Tab（无侧栏）：桌面端侧栏承载品牌与状态，WebUI 响应式侧栏已成熟，否决。
- 备选 C 管线编辑器直嵌库列表（不分离两态）：库项多时画布拥挤、误触率高，否决。
验证：逐页线框走查 + §5 功能树覆盖率核对（每个 L3 有且仅有一个归属位）。
### 4.3 交互决策

选定：WebUI 管线编辑器（ReactFlow）为成熟蓝本，桌面端按其交互模型逐项移植；桌面能力经 ep-core 直调、WebUI 经 daemon API，交互语义对齐、传输层保留差异。

备选与否决理由：
- 备选 A 桌面另起独立交互模型：用户已认定 WebUI 成熟，另起炉灶重复试错且双端行为分叉，否决。
- 备选 B 更换画布库/自研 Web 画布：ReactFlow 生态成熟、皮肤变量完备，替换收益为负，否决（且 §6.2-E 保护条款禁止动其逻辑）。

验证：以任务 13 全量 E2E 测试矩阵为行为基线，重构前后逐项通过；桌面移植项按 §7.3 映射表逐条勾选。

### 4.4 视觉特效与动效决策

选定：辉光（Shadow/box-shadow）+ 局部微渐变 + 三级层深表达科技感；动效 150-200ms ease-out，呼吸辉光 2.4s、连线流光 1.6s。

备选与否决理由：
- 备选 A 大面积毛玻璃 + backdrop-blur：性能开销高，egui 0.31 无此能力，且过强模糊削弱可读性，否决（仅浮层小范围保留）。
- 备选 B 粒子/3D 动效：喧宾夺主、功耗高，与数据仪表盘定位冲突，否决。
- 备选 C 完全无动效（静态）：失去运行态生命力与科技感，否决。
验证：动画开启/关闭两态手动走查；连续运行 10 分钟观察重绘帧率与 REPAINT_WATCHDOG 无告警；egui 降级项（呼吸/流光插值）逐项目视对比。

### 4.5 组件与技术选型决策

选定：WebUI 保留 React+Tailwind v4+shadcn/ui 仅做样式层升级；桌面保留 egui/eframe 0.31 自绘并扩 Palette；令牌为唯一真源（index.css CSS 变量 ↔ palette.rs 字段对照见 §3.5）。
备选与否决理由：
- 备选 A 引入新 Web 组件库（AntD/MUI）：与既有 shadcn/Tailwind 冲突，迁移成本高，否决。
- 备选 B 桌面引入 WebView 混合渲染管线页：破坏单进程内嵌优势、引入跨进程复杂度，否决。
- 备选 C 双端各维护一套令牌：必然漂移，违背统一设计语言目标，否决。

回退策略：所有决策以令牌为落点，若某决策验证不通过，仅需回退对应令牌/组件样式，不动 IA 与数据流。


## 5. 功能清单与层级梳理（IA 唯一依据）

盘点基础为代码：WebUI 前端（pages/ + api/client.ts + hooks/）、daemon API 面（ep-daemon/src/api/ 路由）、桌面端（pages/ + main.rs 编排）。本章是逐页重设计（§7）与波次排期（§11）的唯一功能依据，逐页章节只引用本章、不再重复罗列。

### 5.1 统一功能树（L1 导航 → L2 页面区块 → L3 功能）

L1 按五页导航组织。标注约定：✓=已有，✗=缺失，（平台差）=两端形态不同但语义等价。

**L1 仪表盘**
- L2 统计条带：L3 设备数/模块数/任务数（桌面 ✓，WebUI ✗→补）。
- L2 设备卡：L3 GET /devices 列表、VRAM 与利用率、状态徽章（两端 ✓）。
- L2 模块摘要表（两端 ✓）。
- L2 依赖摘要 GET /deps（两端 ✓）。
- L2 更新机制：WebUI 轮询+WS 指示器；桌面轮询（平台差，保留）。

**L1 模块**
- L2 筛选：L3 tag chips 多选（两端 ✓）、关键词搜索（WebUI ✓，桌面 ✗→补）。
- L2 模块卡：L3 列表/状态、启停、变体选择与激活 PUT variant、激活徽章（两端 ✓）。
- L2 模型管理：L3 变体下载、下载进度、取消下载、检查更新、更新提示、模型删除、模型导入、tag 编辑（WebUI ✓；桌面经 ep-core 直调 ✓，无浏览器上传，用原生文件选择）。
- L2 直跑抽屉：L3 capability 选择/输入文件/参数表单/提交 execute/single（两端 ✓）。
- L2 日志抽屉（两端 ✓）。
- L2 整合包：L3 列表/导入/构建/导出/卸载（WebUI client ✓；桌面经 ep_pack 直调 ✓，入口分散→归拢）。
- L2 模块导出对话框：圈选变体+勾选管线+bundle/reference（桌面 ✓；WebUI 以 packs/build 等价）。
- L2 模块详情：WebUI 独立页 module-detail.tsx ✓，桌面卡内→裁撤独立页统一为抽屉。

**L1 管线**
- L2 库：L3 列表/打开/删除/新建（两端 ✓，形态升级为两态，见 §7.3）。
- L2 画布：L3 节点拖拽/端口连线/删除/选择/缩放平移/网格/MiniMap/Controls/palette 拖放（WebUI ✓ 全套；桌面部分 ✓，MiniMap/Controls/框选待补，见 §7.3 映射表）。
- L2 参数面板（两端 ✓）。
- L2 校验（两端 ✓，呈现对齐）。
- L2 保存 PUT /pipelines/{id}（两端 ✓；桌面另有原生路径浏览，保留为桌面专属）。
- L2 执行对话框 POST /pipelines/execute（两端 ✓，字段与校验链对齐）。
- L2 VRAM 预算 POST /pipelines/vram-budget（WebUI ✓，桌面 ✗→补）。
- L2 管线任务与取消（WebUI client ✓；桌面弱→补）。

**L1 任务**
- L2 统计区块：管线任务/运行服务/全部模块（WebUI 统计条带 ✓，桌面三段 section ✓，呈现统一）。
- L2 状态筛选（两端 ✗→新增 SegmentedTabs）。
- L2 任务卡手风琴：节点状态/错误复制/产物获取（两端 ✓；WebUI http 下载，桌面打开文件夹，平台差保留）。
- L2 任务取消（daemon 路由 ✓；桌面按钮 ✓；WebUI client 未接→补）。
- L2 实时进度 ws/progress、ws/logs（WebUI ✓；桌面轮询，平台差保留）。

**L1 设置**
- L2 Sections：WebUI 10 段（server/general/compute/ports/models/network/python/packs/pipeline/advanced）；桌面 8 段（general/compute/models/ports/network/python/pipeline/ui）。
- L2 页级动作：dirty 指示/重置/保存 PUT /config（WebUI ✓ putConfigPatch；桌面 ✓ 已提升动作条）。
- L2 校验：validateConfig+scroll to error（WebUI ✓；桌面弱→补）。
- L2 桌面专属：语言切换、UI 段（主题/窗口）。WebUI 专属：header 主题切换、packs 段（→迁往模块页）。

### 5.2 差异矩阵与对齐决策

类型标注：补缺失/行为不一致/平台差保留/裁撤/归拢。

| L3 功能 | WebUI | 桌面 | 类型 | 对齐决策 |
|---|---|---|---|---|
| 仪表盘统计条带 | ✗ | ✓ | 缺失 | 补 WebUI |
| 任务状态筛选 | ✗ | ✗ | 双缺 | 两端新增 SegmentedTabs |
| 任务取消按钮 | ✗（client 未接） | ✓ | 缺失 | 补 WebUI TaskCard |
| VRAM 预算实时 | ✓ | ✗ | 缺失 | 桌面补（W3，ep-core 直调） |
| 模块关键词搜索 | ✓ | ✗ | 缺失 | 桌面补（W2） |
| 模块详情独立页 | ✓ | ✗（卡内） | 不一致 | 裁撤 WebUI 独立页→统一抽屉/Sheet |
| 模型浏览器上传 | ✓ | N/A | 平台差 | 保留（桌面原生文件选择更优） |
| 产物获取 | http 下载 | 打开文件夹 | 平台差 | 保留 |
| WS 指示器 | ✓ | N/A（内嵌） | 平台差 | 保留 |
| 管线路径浏览 | ✗ | ✓ | 桌面专属 | 保留（文件系统语义） |
| MiniMap/Controls/框选 | ✓ | ✗ | 缺失 | 桌面按 §7.3 映射表补 |
| 整合包入口 | 分散 settings+modules | modules 导出/导入 | 分散 | 归拢为模块页 L2「整合包」 |
| 启动检查更新 | config+toast | settings+启动编排 | 分散 | 层级纠偏 §5.3 |
| 直跑 | ✓ | ✓ | 已对齐 | 保留 |

### 5.3 归属与层级纠偏

1. 保存配置/重新加载：曾埋于桌面设置卡内→已提升为页级动作条（现状已对齐）；WebUI 位于页头 actions；两端位置统一。
2. 检查全部更新：现散于启动编排（main.rs）与 toast→归位为模块页页头次要动作（低频全局动作），设置保留开机检查开关。
3. 整合包：WebUI settings.packs 段裁撤，import/build/export/uninstall 归拢到模块页「整合包」抽屉；桌面导出/导入对话框保留并视觉对齐。
4. 管线库：由工具栏下拉（WebUI）/内联列表（桌面）提升为 L2 库视图（两态，见 §7.3）。
5. 桌面语言/UI 段为桌面专属 L2，保留，不与 WebUI header 强行对齐。

### 5.4 取舍原则（平台定位：管理调度平台，能力由模块开放）

- 核心常驻（始终可见）：启停、变体激活、直跑、库打开/保存、管线执行、任务监控、设置保存。
- 高频次级（一键可达）：日志、tag、模型下载/取消/查更新、筛选。
- 高级低频（收进抽屉/上下文菜单/高级区）：整合包 build/export、模型上传/导入、端口、python 路径、advanced、检查全部更新。
- 规则：低频不占主屏；破坏性操作（卸载/删除）两步确认；UI 不硬编码业务能力，全部由模块 manifest/schema 驱动。

### 5.5 证据锚点

- daemon API 面：ep-daemon/src/api/{health,devices,modules,config,pipelines,execute,models,upload,tasks,deps,packs}.rs 与 ws/{all,progress,logs}.rs。
- WebUI 客户端：ep-webui/frontend/src/api/client.ts（api 对象全量）、hooks/use-direct-exec.ts、use-config.ts、use-model-downloads.ts。
- 桌面端：ep-desktop/src/pages/{dashboard,modules,pipeline_editor,tasks,settings}.rs、main.rs（整合包/模型编排）、app.rs（导航/窗口）。



## 6. WebUI 重构章节

### 6.1 与新设计语言的差距盘点

1. **令牌层**：index.css 缺 surface/glow/gradient 令牌；无 .glow/.glass 工具类。
2. **组件层**：Button/Card/Badge/Input/Dialog/Progress 均为 shadcn 默认扁平样式，无发光/聚焦辉光；进度条无渐变。
3. **布局层**：Header/Sidebar/PageContainer 结构良好，仅缺品牌化视觉（渐变 Logo、激活态发光条）。
4. **页面层 IA 差距**：仪表盘缺统计条带（桌面端有）；管线页是「单编辑器 + 库下拉」而非「库优先两态」；任务页缺状态筛选。
5. **文档层**：DESIGN_SYSTEM.md 第 7 节「业务页面为占位实现」等描述已严重过时，须升版为 v2。

### 6.2 WebUI 重构清单

**A. 令牌与工具层**（index.css）：
- 新增 §3.1 令牌；新增工具类 .glow-primary/.glow-status/.glass/.text-gradient/.bg-gradient-accent。

**B. shadcn 组件样式升级**（components/ui/，仅样式与少量行为增强，不换组件库）：
- button.tsx：primary 加 glow + hover 高光；新增 size 微调；危险态红色发光。
- card.tsx：hover-glow 变体；glass 变体（浮层用）。
- badge.tsx：状态点辉光；过渡态 pulse 保留。
- input.tsx/select.tsx/switch.tsx：聚焦 ring-glow；switch 打开态主色发光。
- dialog.tsx/sheet.tsx：遮罩加深 + 内容 glass 化（backdrop-blur）。
- progress.tsx：渐变填充；queued 脉冲。
**C. 布局层**（components/layout/）：
- header.tsx：WS 指示器加状态辉光；主题切换按钮样式。
- sidebar.tsx：激活项左侧 2px 渐变竖条 + primary/10 底；Logo 字标渐变。
- page-container.tsx：间距对齐 §3.3（px-6 py-4 已符合，补 description 弱化样式）。
**D. 页面层重构清单**（仅 IA 与内容，视觉跟随 A/B 令牌）：
- dashboard.tsx：新增统计条带 StatStrip（在线设备/已载模块/运行任务），与桌面端 IA 对齐；设备卡统一 StatusBadge。
- tasks.tsx：新增状态筛选 SegmentedTabs（全部/运行中/排队/完成/失败）；保留统计条带与 TaskCard 手风琴。
- pipeline.tsx：改为「库优先两态」（默认库视图，点卡片进入编辑器）；仅改入口 IA 与页面壳，编辑器内部见下条硬约束。
- modules.tsx：保留现有 IA（已成熟）；筛选 chips、变体 Select、Dialog 组仅视觉层统一。
- settings.tsx：保留 10 Sections 与单列 max-w-4xl（双列卡片为桌面端专属决策，见 §7.5）；仅升级控件聚焦辉光与保存条样式。

**E. 硬约束：管线编辑器保护条款**

WebUI 管线编辑器（pipeline.tsx 的 PipelineEditor 与 pipeline-node.tsx）是已认定的成熟蓝本，本次重构对其只允许样式层改造：
- 允许：--xy-* 皮肤变量换肤、节点卡描边与辉光、连线颜色、Controls/MiniMap 外观、ExecuteDialog 视觉样式。
- 禁止：节点拖拽/连线/删除/选择逻辑、缩放平移、参数面板逻辑、ExecuteDialog 提交逻辑、库加载、校验、TOML 序列化。
- 验收：重构前后全量 E2E 管线编辑器用例须全部通过并行为对比（任务 13 测试矩阵为基线）。

## 7. 桌面端逐页重设计（egui）

通用约定：所有页面视觉引用 §3 统一设计语言；卡片/页头使用 §10 修正后的令牌；列数随 §8.1 断点变化；页面功能以 §5 功能树为依据；结构统一为 page_header（标题+描述+动作）+ 可滚动内容区。

### 7.1 仪表盘

现状问题：统计条带、设备卡网格、模块表格已有（溢出问题任务 8 已修），但令牌陈旧、状态无辉光、与 WebUI 缺双向 IA 对齐（WebUI 无统计条带，已在 §6.2-D 补齐对端）。
目标 IA：统计条带（设备/模块/任务三个 mono 大数字）→ 设备卡网格（1/2/3 列随断点）→ 模块表格 → 依赖摘要区。
关键交互：自动刷新（沿用现有轮询）；设备卡 hover 辉光；异常设备卡 status-error 描边辉光；VRAM/显存进度条渐变填充（egui 用分段色带近似）。
与 WebUI 差异：桌面端表格列宽更宽、可显示更多列（如驱动版本）；WebUI 有 ping 动画 pill，桌面端以状态徽章静态表达。
涉及文件：pages/dashboard.rs、ui/components.rs（card_frame/badge/progress 令牌升级）。
### 7.2 模块

现状问题：列表、变体选择、Run/Logs/Tags 抽屉、tag chips 筛选已有（经代码核实），但与 WebUI 的视觉一致性、信息密度与操作菜单观感落后。
目标 IA：筛选行（tag chips + 搜索框）→ 模块卡网格（1/2 列随断点，卡内含变体 Select：name·状态·体积·VRAM）→ 右侧 Drawer（沿用现有 Run/Logs/Tags 三类，视觉升级）→ 导入/导出/卸载 ConfirmDialog。
关键交互：tag chip 多选过滤；卡片点击进入详情 Drawer；日志 Drawer 带级别筛选；卸载两步确认（输入模块名）。
与 WebUI 差异：WebUI 用 Sheet/Dialog 浮层（backdrop-blur），桌面端 Drawer 降级为右侧固定面板滑入（egui 无原生 Sheet，动画用位移插值近似）。
涉及文件：pages/modules.rs、新增 ui/drawer.rs、ui/components.rs（chip/confirm_dialog 升级）。
### 7.3 管线（核心：库优先两态 + WebUI 蓝本移植）

现状问题：进入即编辑器工具栏 + 空态（任务 26 已加一键加载列表，本方案将其升格为正式库视图）；画布交互与 WebUI 蓝本存在行为差距。
目标 IA：两态切换。态 A 库视图（默认）：管线文件卡片网格（名称/节点数/更新时间）+ 新建；态 B 编辑器视图：工具栏 + 左侧 palette + 画布 + 右侧参数面板 + 状态栏，结构与 WebUI PipelineEditor 一致。
状态机：库 --打开/新建--> 编辑器 --返回--> 库；未保存返回触发 ConfirmDialog；编辑器内 Esc 不直接退出（防误触）。

**WebUI 管线编辑器能力清单 → 桌面端映射表**（蓝本=pipeline.tsx 与 pipeline-node.tsx，行号锚点见附录）：

| # | WebUI 能力（证据） | 桌面映射状态 | egui 实现方式 |
|---|---|---|---|
| 1 | 节点拖拽（onNodesChange） | 已有，需校准 | 指针抓取+位置累加；校准缩放后的坐标变换 |
| 2 | 端口拖拽连线 out→in（onConnect+Handle） | 已有，需校准 | 端口热区拖拽状态机+贝塞尔预览线（draw_bezier_preview 已有） |















| 3 | 画布缩放/平移（wheel+drag） | 弱/缺 | 替代实现：自定义 zoom 变换（scale+offset），egui 无画布变换 API |
| 4 | Background(Dots) 点阵网格 | 已有，换色 | draw_grid 改点阵样式，色取 --grid-dot |
| 5 | Controls（放大/缩小/适配 fitView） | 缺 | 自定义按钮组；fit=包围盒居中缩放（替代实现） |
| 6 | MiniMap | 缺 | 替代实现：画布角落实时缩略框 + 点击跳转视口 |
| 7 | 删除键删除（deleteKeyCode Backspace/Delete） | 需补 | egui 键盘事件捕获，删除选中节点/边 |
| 8 | 选择/框选多选（onSelectionChange） | 单选有 | 框选（marquee）矩形选择替代实现 |
| 9 | palette 拖入画布新增节点（onDrop） | 需补 | egui::DragAndDrop 或自定义拖拽载荷 + 屏幕坐标→画布坐标换算 |
| 10 | 参数面板（节点配置） | 已有 | 对齐 WebUI 字段控件形态与校验规则 |
| 11 | ExecuteDialog（字段/file_input 校验/VRAM 阻断） | 执行流有 | 改模态对话框，执行前校验链对齐 |
| 12 | PipelineLibraryBar 库下拉 | 任务 26 列表有 | 升格为态 A 库视图（本方案两态改造） |
| 13 | validate 校验（空画布/缺输入输出，toast） | 已有 | 对齐错误文案与 toast 呈现 |
| 14 | TOML 双向序列化 {from:[node,port],to:[node,port]} | 已有 | 保持格式兼容，两端读写同一文件 |
| 15 | 运行态可视化（节点状态辉光） | 已有，需升级 | 节点状态辉光（§1 主张 2/5）+ 连线流光近似 |

注：第 3/5/6/8/9 行为 egui 无现成对应物、需自研替代的实现点；其余为对齐/换肤。桌面编辑器移植全程以 WebUI 行为为准，验收对照任务 13 E2E 矩阵。

涉及文件：pages/pipeline_editor.rs（129KB，建议 W3 结构拆分为 pipeline/{library,editor,canvas}.rs，仅拆分不改逻辑）、app.rs（两态导航状态）。

### 7.4 任务

现状问题：统计三段、任务卡、取消按钮、产物区已有（经代码核实），但缺状态筛选；产物预览与画布交互对照缺失（已知问题）；视觉令牌陈旧。
目标 IA：统计条带（mono 大数字）→ SegmentedTabs 状态筛选（全部/运行中/排队/完成/失败/已取消）→ 运行中服务卡网格 → 任务卡手风琴（节点状态/错误复制/产物获取）。
关键交互：筛选状态会话内保持；运行中任务显示取消按钮；错误一键复制；产物「打开文件夹」（桌面语义，保留平台差）。
与 WebUI 差异：WebUI 产物走 http 下载、桌面打开文件夹；WebUI 需补任务取消按钮（§5.2 矩阵）。
涉及文件：pages/tasks.rs、ui/components.rs（SegmentedTabs 新增）。

### 7.5 设置

现状问题：两处 checkbox 空标签实证缺陷——settings.rs 第 105 行 check_updates 与第 155 行 allow_overcommit 均传入空字符串，用户看不到控件文案；端口用裸 DragValue 观感静态；单列卡片稀疏。
目标 IA：双列卡片网格（>=1240px 双列，以下单列）；Section 顺序对齐桌面 8 段（general/compute/models/ports/network/python/pipeline/ui）；页头动作条（dirty 指示+重置+保存/重新加载，已提升保留）。
关键交互：修复两处空标签 checkbox 补文案与描述；端口控件改 DragValue+可编辑 TextEdit 混合观感；校验失败滚动定位首个错误（对齐 WebUI validateConfig）。
与 WebUI 差异：WebUI 保留单列 max-w-4xl，桌面双列卡片（宽屏密度决策，见 §4.2）；桌面专属语言切换与 UI 段保留。
涉及文件：pages/settings.rs、ui/components.rs（SwitchRow/FormRow 新增）。

## 8. 窗口自适应与默认尺寸

### 8.1 桌面端断点策略

现状：COMPACT_WIDTH_THRESHOLD=1000.0（<1000 侧栏图标化 68px），1000-1280 区间无中间断点，网格列数与侧栏状态不匹配。

提案——三档断点（按窗口内容区逻辑宽度判断）：

| 断点 | 宽度 | 侧栏 | 网格列数 | 其他 |
|---|---|---|---|---|
| 宽屏 | >=1280 | 180px 带标签 | 设备卡 3 列/模块 2 列/设置双列 | 管线右栏 320px |
| 中间（新增） | 1000-1279 | 180px 带标签 | 设备卡 2 列/模块 2 列/设置双列 | 管线右栏 280px 可折叠 |
| 紧凑（已有） | <1000 | 68px 图标化 | 一律 1 列 | 统计条带折行、表格降级卡片列表 |

规则：断点只改回流布局，不删减任何功能（功能等价，呼应 §5.4）；阈值按逻辑像素；管线画布不参与断点（始终占满剩余宽度）。

### 8.2 默认尺寸与最小尺寸

现状：main.rs ViewportBuilder 默认 1280×800、最小 720×480；window-state.json 持久化上次窗口 rect（本机会话实测 2560×1369，交接所述 1320×736 为更早一次退出的快照——机制记录的是「上次退出尺寸」而非固定值）。

问题：现默认 1280×800 的高度 800 超出主流 1366×768 笔电扣除任务栏后的可用内高（约 720-730），首次启动即被裁切或触发收缩。

推荐：
- 默认窗口 1320×760：在 1366×768（最大存量的入门分辨率）扣任务栏后完整容纳；在 1920×1080 占约 69% 宽，内容密度适中；且与用户实际使用的 1320×736 习惯贴近。
- 最小窗口提升为 860×560（自 720×480）：保证紧凑断点降级后核心布局仍可用，低于此宽度表格/卡片降级过狠。
- 恢复逻辑：优先 window-state.json 的 rect；超屏沿用 fit_window_to_screen 收缩到显示器 92%（已有）；补「保存 rect 不被任何现存显示器覆盖时，回退主显示器居中 + 默认尺寸」（现有缺口，见 §13 风险）。

### 8.3 副屏与高 DPI

- 副屏：退出时随 rect 记录所在显示器；启动校验 rect 与任一现存显示器工作区相交，否则按 §8.2 回退——覆盖「拔副屏/换分辨率」场景。
- 高 DPI：eframe/winit 自动缩放，egui 逻辑像素无需手动适配字号间距；窗口持久化存逻辑像素，跨 DPI 显示器恢复由 winit 换算。
- 图标：现 emoji 不受缩放影响；若后续换矢量图标须提供 2x 资产，避免高 DPI 模糊。

### 8.4 WebUI 响应式断点与移动端取舍

现状断点：sm 640 / lg 1024（汉堡→Drawer 导航）/ xl 1280（模块两列）；header 在 <sm 隐藏 WS 文本。

规范化：
- <640（移动）：侧栏 Drawer（已有）；统计条带折行；表格降级卡片列表；管线编辑器只读预览。
- 640-1023（平板/窄窗）：侧栏 Drawer；网格 1-2 列自适应。
- >=1024：侧栏常驻完整布局。
移动端取舍结论：仅保证「看 + 轻操作」（仪表盘/任务/模块的查看与启停、设置编辑）；管线编辑器不做 <1024 触控适配（ReactFlow 技术可行但参数面板空间不足、维护成本大于收益），移动端呈现只读画布 + 引导文案。此结论同步写入 §12 不做清单。

涉及文件：桌面 main.rs（ViewportBuilder/load_window_state）、app.rs（断点/fit_window_to_screen/track_window_rect）、runtime/window-state.json；WebUI layout/sidebar.tsx、header.tsx、各页 grid 类。

## 9. 组件层清单（双端）

同名组件两端同义；新增组件必须同步 i18n 文案（WebUI react-i18next 14 命名空间 / 桌面 ep-core tr）。

| 组件 | WebUI 现状与动作 | 桌面现状与动作 |
|---|---|---|
| StatStrip 统计条带 | 无→新增 shared/stat-strip.tsx | 已有雏形→令牌升级（mono 大数字+大写标签） |
| StatusBadge | badge variants→加四态色与辉光 | badge 已有→换四态色+辉光 |
| SegmentedTabs | 无→新增（任务筛选） | 无→新增（任务筛选） |
| Drawer/Sheet | shadcn sheet 已有→glass 化 | Run/Logs/Tags 抽屉已有→视觉升级，抽 ui/drawer.rs 复用 |
| ConfirmDialog | AlertDialog 已有→视觉升级 | confirm_dialog_with_lang 已有→视觉升级 |
| EmptyState | shared 已有→保留 | 已有→保留（管线库一键加载任务 26 已加） |
| Table/DataTable | table 已有→令牌化列距 | 自绘 table→令牌化列宽 |
| SwitchRow/FormRow | settings 复用 label+控件→统一 | 无（空标签缺陷）→新增，修复 overcommit/check_updates 空标签 |
| ProgressBar | progress→渐变填充+queued 脉冲 | 简陋→分段渐变近似 |
| Toast/InfoBar | sonner→保留 | toast→保留 |
| MiniMap/Controls | ReactFlow 内建→换肤 | 无→自研新增（W3） |
| 图标体系 | lucide-react→保留 | emoji→短期保留，矢量图标长期评估（§13 风险） |

## 10. 主题与令牌补齐/修正

1. palette.rs 修正：CARD_ROUNDING 12→10（对齐 WebUI --radius-lg）；卡片内边距 16→24；新增三级层深 bg_base/bg_card/bg_raised、border_glow/border_glow_strong、accent_from/accent_to、grid_dot；badge_bg 对齐 --muted。
2. index.css：新增 --border-glow/--border-glow-strong/--surface-glass/--grid-dot/--duration-fast/--duration-base/--ease-standard 与工具类 .glow-primary/.glass/.text-gradient/.bg-gradient-accent；background/card/popover 重定为三级层深。
3. 四态状态色重定值（breaking）：--status-ready/-running/-error/-stopped 改为 #4ADE80/#22D3EE/#FB7185/#94A3B8（§1.2 权威）；桌面 service_status 映射同步；所有状态色消费点（badge/点/进度/toast）须回归目检。
4. 紧凑密度规则：<1000px 时卡片内边距 24→16、表格行高收紧、间距比例约 0.75，保证紧凑模式信息密度。
5. 浅色主题：深空暗色为第一主题；浅色保留既有令牌但仅跟随四态色与语义色，辉光/玻璃拟态在浅色下弱化（白底不宜发光）。
6. DESIGN_SYSTEM.md 升版 v2：与 W0 令牌落地同步，删除「业务页面为占位实现」等过时描述。

## 11. 分阶段实施计划

令牌先行，页面随后，管线最后（依赖令牌与组件）。桌面回归基线 = 76 个 #[test]；WebUI = build + 任务 13 E2E 矩阵。标注〔并行〕= 两端可同时进行。

### W0 令牌与视觉基线〔两端并行〕

- WebUI：index.css 新增令牌/工具类、四态色重定值、三级层深（§6.2-A、§10）。
- 桌面：palette.rs/theme.rs 修正（CARD_ROUNDING、内边距、层深、glow 字段）、四态色映射（§10）。
- 文档：DESIGN_SYSTEM.md v2。
- 改动量：S。验证：桌面 76 单测全绿 + WebUI build 通过 + 双端截图对照概念图取色。依赖：无。风险：四态色 breaking 需全量目检状态消费点。

### W1 设置 + 仪表盘〔两端并行〕

- WebUI：仪表盘新增 StatStrip（§6.2-D）；设置控件聚焦辉光、保存条样式。
- 桌面：设置双列卡片 + 修复两处空标签 checkbox + 端口可编辑观感 + 校验滚动定位（§7.5）；仪表盘令牌升级 + 统计条带 mono。
- 组件：新增 StatStrip、SwitchRow/FormRow（§9）。
- 改动量：M。验证：桌面 76 单测 + 设置保存/重载手测 + WebUI 设置 E2E。依赖：W0。风险：设置校验逻辑两端一致性。

### W2 模块 + 任务〔两端并行〕

- WebUI：任务页新增 SegmentedTabs 状态筛选 + 任务取消按钮接线（§6.2-D、§5.2）；模块视觉统一；裁撤 module-detail 独立页并入抽屉。
- 桌面：模块页补关键词搜索、视觉对齐（§7.2）；任务页新增 SegmentedTabs（§7.4）。
- 组件：新增 SegmentedTabs；ui/drawer.rs 抽取复用。
- 改动量：M。验证：桌面 76 单测 + 模块直跑/日志/标签手测 + WebUI 任务 E2E。依赖：W0。风险：module-detail 裁撤的路由与深链兼容。

### W3 管线两态 + 收敛〔高风险波次〕

- WebUI：管线页改库优先两态入口（§6.2-D），编辑器内部仅样式层（§6.2-E 保护条款）。
- 桌面：库视图升格（态 A）+ 编辑器按 §7.3 映射表补齐缩放/平移/MiniMap/Controls/框选/删除键等；pipeline_editor.rs 结构拆分。
- 依赖：W0 令牌、W2 组件库。改动量：L。
- 验证：桌面 76 单测 + 管线 E2E 全用例（任务 13 矩阵）重构前后行为对比 + 渲染冻结复验（最大化/重启场景，REPAINT_WATCHDOG）+ VRAM 预算桌面接线验证。
- 回归面汇总：桌面 76 单测（其中网格数学 4 个、keyboard_scroll 2 个直接受布局/令牌改动影响）；WebUI 全量 E2E；i18n 14 命名空间 × 2 语言新增文案同步。

## 12. 不做清单

- 不改动 WebUI 管线编辑器交互逻辑（仅样式层，§6.2-E）。
- 不做移动端管线编辑（<1024 只读预览，§8.4）。
- 不引入新组件库（WebUI 保留 shadcn，桌面保留 egui 自绘）。
- 不在桌面实现渐变画刷/backdrop-blur（egui 0.31 不支持，用辉光/分段插值降级）。
- 不改 daemon/API 层与 ep-core 业务逻辑（仅 UI 层）。
- 不做多主题市场（保留暗/浅两主题）。
- 不重构 i18n 结构（仅新增文案）。
- 不在本方案内更换桌面 emoji 图标体系（仅评估，§13）。

## 13. 风险

1. egui 能力天花板：无渐变画刷/backdrop-blur，科技感靠辉光与插值近似，视觉与 WebUI 存在可感知差距——接受为平台差。
2. pipeline_editor.rs 129KB 单文件：W3 拆分为 library/editor/canvas 三模块有回归风险，须仅结构拆分不改逻辑并全量 E2E 护航。
3. 76 单测回归：网格数学 4 个、keyboard_scroll 2 个对布局/令牌敏感，W0/W1 即可能触发。
4. 渲染冻结回归：任务 7/28 已修 P0 冻结，任何窗口/布局改动须复验最大化与重启场景。
5. 窗口恢复缺显示器覆盖校验：拔副屏后 window-state.json 的 rect 可能落在不存在的显示器上（§8.2 已列修复项）。
6. 四态状态色 breaking 重定值：所有状态消费点需目检回归，漏改会导致语义错乱。
7. i18n 双轨同步遗漏：新增组件/断点文案须同时落 WebUI 14 命名空间与桌面 ep-core tr，缺一端即回归失败。
8. 桌面 emoji 图标与 WebUI lucide 风格不一致：短期保留，长期需矢量图标方案，否则科技感打折（列入后续评估）。

## 附录

**A. egui 0.31.1 能力证据（实测）**：Cargo.lock 锁定 egui/eframe/epaint 0.31.1；epaint 源码无 Brush/Gradient（无渐变画刷）；egui::DragAndDrop 可用；epaint::Shadow 支持 color/blur/spread（彩色辉光可行）；无 backdrop-blur。

**B. 测试基线**：ep-desktop 76 个 #[test]；WebUI 以任务 13 全量 E2E 矩阵为行为基线。

**C. i18n**：WebUI react-i18next 14 命名空间 × en/zh-CN；桌面 ep-core tr()。新增文案两端同步。

**D. 关键文件锚点**：
- WebUI：ep-webui/frontend/src/pages/{dashboard,modules,module-detail,pipeline,tasks,settings}.tsx、api/client.ts、components/{ui,shared,layout}/、index.css。
- 桌面：ep-desktop/src/{main,app}.rs、pages/{dashboard,modules,pipeline_editor,tasks,settings}.rs、ui/{palette,theme,components}.rs。
- daemon：ep-daemon/src/api/、ws/。
**E. 美学基准概念图**：C:\Users\PegionFish\.qoder-cn\vibe_images\entrypoint-unified-ui-concept_1786206794.png

**F. 窗口状态机制**：runtime/window-state.json 记录「上次退出」的 rect（非固定值）；本会话实测 2560×1369，交接快照 1320×736。

---
状态：草案（待评审）。范围：仅设计方案，不含代码改动。


