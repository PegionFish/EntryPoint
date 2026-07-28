# EntryPoint WebUI 设计系统

本文档定义 EntryPoint WebUI 的视觉规范、组件行为与交互模式，作为前端实现的唯一权威参考。所有页面与组件必须遵循本规范，保证整个应用视觉与交互的一致性。

- **默认主题**：深色（dark）
- **界面语言**：简体中文
- **组件库**：shadcn/ui（new-york 风格）+ Tailwind CSS v4
- **图标库**：lucide-react

---

## 1. 色彩系统

颜色通过 CSS 变量定义于 `src/index.css`，并在 `@theme inline` 中映射为 Tailwind 颜色令牌（如 `bg-background`、`text-foreground`、`border-border`）。深色主题为默认；浅色主题通过移除 `<html>` 上的 `dark` 类切换。

### 1.1 深色主题（默认）

| 令牌 | HSL | 用途 |
| --- | --- | --- |
| `--background` | `hsl(240 10% 3.9%)` | 页面整体背景 |
| `--foreground` | `hsl(0 0% 98%)` | 主文本 |
| `--card` | `hsl(240 6% 10%)` | 卡片 / 侧边栏 / 顶栏背景 |
| `--card-foreground` | `hsl(0 0% 98%)` | 卡片内文本 |
| `--popover` | `hsl(240 10% 3.9%)` | 弹层 / 下拉菜单背景 |
| `--popover-foreground` | `hsl(0 0% 98%)` | 弹层文本 |
| `--primary` | `hsl(217 91% 60%)` | 主操作（蓝色），按钮 / 链接 / 焦点环 |
| `--primary-foreground` | `hsl(0 0% 100%)` | 主操作上的文本 |
| `--secondary` | `hsl(240 3.7% 15.9%)` | 次要按钮背景 |
| `--secondary-foreground` | `hsl(0 0% 98%)` | 次要按钮文本 |
| `--muted` | `hsl(240 3.7% 15.9%)` | 弱化区块背景 |
| `--muted-foreground` | `hsl(240 5% 64.9%)` | 弱化文本（说明、占位） |
| `--accent` | `hsl(240 3.7% 15.9%)` | 悬停高亮背景 |
| `--accent-foreground` | `hsl(0 0% 98%)` | 悬停高亮文本 |
| `--destructive` | `hsl(0 84% 60%)` | 危险操作（删除 / 停止） |
| `--destructive-foreground` | `hsl(0 0% 100%)` | 危险操作文本 |
| `--border` | `hsl(240 4% 16%)` | 边框 / 分割线 |
| `--input` | `hsl(240 4% 16%)` | 输入框边框 |
| `--ring` | `hsl(217 91% 60%)` | 焦点环 |
| `--radius` | `0.625rem` | 基础圆角 |

### 1.2 浅色主题

| 令牌 | HSL |
| --- | --- |
| `--background` | `hsl(0 0% 100%)` |
| `--foreground` | `hsl(240 10% 3.9%)` |
| `--card` | `hsl(0 0% 100%)` |
| `--primary` | `hsl(217 91% 60%)` |
| `--secondary` / `--muted` / `--accent` | `hsl(240 4.8% 95.9%)` |
| `--muted-foreground` | `hsl(240 3.8% 46.1%)` |
| `--border` / `--input` | `hsl(240 5.9% 90%)` |

> 其余浅色令牌见 `src/index.css` 的 `:root` 块。主色（primary）在两套主题中保持一致的蓝色。

### 1.3 状态色

状态色用于模块运行状态、连接指示与进度反馈，定义为独立令牌（`--status-*`），并映射为 `text-status-*` / `bg-status-*` 等 Tailwind 类。

| 状态 | 令牌 | HSL | 语义 |
| --- | --- | --- | --- |
| Running（运行中） | `--status-running` | `hsl(142 71% 45%)` | 绿色，服务正常运行 |
| Stopped（已停止） | `--status-stopped` | `hsl(240 5% 65%)` | 灰色，已停止 |
| Starting（启动中） | `--status-starting` | `hsl(217 91% 60%)` | 蓝色，过渡态（脉冲动画） |
| Preparing（准备中） | `--status-preparing` | `hsl(45 93% 47%)` | 黄色，过渡态（脉冲动画） |
| Error（错误） | `--status-error` | `hsl(0 84% 60%)` | 红色，故障 |
| NotReady（未就绪） | `--status-notready` | `hsl(240 4% 35%)` | 深灰，依赖缺失 / 未就绪 |

状态元信息（中文标签 + 颜色类 + 是否过渡态）集中在 `src/lib/constants.ts` 的 `STATUS_COLORS`，通过 `statusMeta(status)` 获取，自动归一化大小写与 `notready` → `not_ready`。

---

## 2. 字体排印

字体栈定义于 `src/index.css` 的 `@theme` 块：

- **正文（sans）**：`system-ui, -apple-system, "Noto Sans SC", "Microsoft YaHei", sans-serif`
- **等宽（mono）**：`ui-monospace, "Cascadia Code", "Fira Code", "Noto Sans Mono CJK SC", monospace`

### 字号层级

| 层级 | 规格 | 用途 |
| --- | --- | --- |
| 页面标题 H1 | `text-xl font-semibold tracking-tight`（20px） | `PageContainer` 标题 |
| 区块标题 H2 | `text-base font-semibold`（16px） | 卡片 / 分区标题 |
| 正文 | `text-sm`（14px） | 表格、表单、正文内容 |
| 辅助文本 | `text-xs text-muted-foreground`（12px） | 说明、时间戳、页脚 |
| 大数字 | `text-4xl font-bold`（36px） | 仪表盘统计数字 |

行高默认 `leading-normal`；中文排版开启 `antialiased`。等宽字体用于日志查看器、路径、端口号、数值。

---

## 3. 布局模式

整体采用「顶栏 + 左侧导航 + 内容区」三段式布局，见 `src/App.tsx`。

```
┌──────────────────────────────────────────────┐
│  Header（h-14）                                │
├────────────┬─────────────────────────────────┤
│  Sidebar   │  Content（PageContainer）         │
│  (w-56)    │  ┌───────────────────────────┐   │
│            │  │ 标题栏（title + actions）    │   │
│            │  ├───────────────────────────┤   │
│            │  │ 可滚动内容区 (p-6)          │   │
│            │  └───────────────────────────┘   │
└────────────┴─────────────────────────────────┘
```

| 区域 | 规格 | 说明 |
| --- | --- | --- |
| Header | `h-14`（56px），`bg-card`，底部边框 | 左：应用名「EntryPoint」；右：WS 连接指示 + 主题切换 |
| Sidebar | `w-56`（224px），`bg-card`，右侧边框 | 图标 + 文字导航项，底部版本信息 |
| Content | `flex-1`，内部 `overflow-hidden` | 由 `PageContainer` 提供标题栏 + 滚动内容区 |

- 根容器 `flex h-screen flex-col overflow-hidden`，禁止整页滚动；仅内容区滚动。
- 导航项激活态：`bg-primary/10 text-primary`；非激活：`text-muted-foreground`，悬停 `bg-accent text-foreground`。
- 间距基准：内容区内边距 `p-6`（24px），标题栏 `px-6 py-4`。

---

## 4. 组件行为规范

### 4.1 状态徽章（Status Badge）

基于 shadcn `Badge`（`variant="outline"`）+ 状态圆点。

- 结构：`<span class="dot">` + 中文状态文本。
- 配色取自 `statusMeta(status).badge`，圆点取自 `.dot`。
- **过渡态动画**：`starting` / `preparing` 状态圆点附加 `animate-pulse`，提示用户系统正在处理。
- 未知状态回退为灰色「未知」，并原样显示原始状态字符串。

### 4.2 日志查看器（Log Viewer）

- 容器使用 `ScrollArea`，等宽字体 `font-mono text-xs`，深色背景 `bg-background`。
- 每行日志保留原始格式（`whitespace-pre-wrap break-all`）。
- 新日志到达时自动滚动到底部（除非用户已向上滚动查看历史）。
- 实时日志通过 WebSocket 订阅（见 §5.2）；历史日志通过 `GET /api/modules/:id/logs` 拉取。

### 4.3 提示（Toast）

- 使用 `sonner`（`src/components/ui/sonner.tsx` 的 `Toaster`），主题跟随应用主题。
- 操作反馈：成功（绿）/ 错误（红）/ 加载（旋转图标）。
- 危险操作（停止模块、删除）需先经 `Dialog` 确认，再触发 toast 反馈。

### 4.4 卡片（Card）

- `bg-card` + `border` + `rounded-lg`，内边距 `p-6`。
- 卡片标题用 H2 规格，描述用辅助文本。

### 4.5 表格（Table）

- 表头 `text-xs text-muted-foreground uppercase`（可选）；行悬停 `hover:bg-muted/50`。
- 数值列右对齐，状态列使用状态徽章。

### 4.6 空状态与加载

- 加载中：`Skeleton` 占位，避免布局抖动。
- 空数据：居中图标 + 辅助文本说明，必要时提供主操作按钮。
- 未实现页面：`Placeholder` 组件（施工图标 + 「页面开发中」）。

---

## 5. 交互模式

### 5.1 轮询（Polling）

适用于无实时推送需求的概览数据。

| 数据 | 间隔 | 来源 |
| --- | --- | --- |
| 仪表盘设备 / 模块概览 | `ui.dashboard_refresh_secs`（配置，默认 5s） | `GET /api/devices`、`GET /api/modules` |
| 模块状态 | 5s（仅打开详情时） | `GET /api/modules/:id/status` |
| 模型列表 | 按需 + 操作后刷新 | `GET /api/models` |

- 页面不可见（`document.hidden`）时暂停轮询。
- 用户操作（启动 / 停止）后立即刷新一次，再恢复定时轮询。

### 5.2 WebSocket

- 全局单例 `wsManager`（`src/api/ws.ts`），应用启动时在 `App.tsx` 建立连接。
- **自动重连**：指数退避 `1s → 2s → 4s → 8s → … → 上限 30s`，连接成功后重置。
- **状态跟踪**：`idle / connecting / connected / reconnecting / disconnected`，Header 的连接指示器实时反映（绿点=已连接，黄点脉冲=连接/重连中，红点=断开）。
- **消息**：自动 `JSON.parse` 后分发给订阅者；消息带 `type` 判别字段：
  - `type: "log"` → `{ module_id, line }`（实时日志）
  - `type: "progress"` → `{ pipeline_id, node_id, status }`（管线进度）
- 订阅 API：`wsManager.onMessage(fn)` / `onStateChange(fn)`，返回取消订阅函数；React 中通过 `useWsState()` hook 订阅状态。

### 5.3 自动刷新与乐观更新

- 启动 / 停止模块：先乐观更新本地状态为过渡态（starting/stopped），请求完成后以服务端结果校正。
- 配置修改：`PUT /api/config` 成功后刷新本地配置缓存并 toast 提示。

---

## 6. 分类标签映射

模块分类（`category` 字段）到中文标签的映射，定义于 `src/lib/constants.ts` 的 `CATEGORY_LABELS`，通过 `categoryLabel(category)` 获取（未知分类原样返回）。

| category | 中文标签 |
| --- | --- |
| `asr` | 语音识别 |
| `tts` | 语音合成 |
| `denoise` | 降噪 |
| `ocr` | 文字识别 |
| `image` | 图像处理 |
| `video` | 视频处理 |
| `audio` | 音频处理 |
| `translate` | 机器翻译 |
| `llm` | 大语言模型 |
| `other` | 其他 |

---

## 7. 页面与路由

路由定义于 `src/App.tsx`，导航项定义于 `src/components/layout/sidebar.tsx`。

| 路由 | 页面 | 导航标签 | 图标 | 说明 |
| --- | --- | --- | --- | --- |
| `/` | DashboardPage | 仪表盘 | `LayoutDashboard` | 系统总览、设备与模块实时状态 |
| `/modules` | ModulesPage | 模块 | `Puzzle` | 模块启动 / 停止 / 日志 |
| `/pipeline` | PipelinePage | 管线 | `GitBranch` | 可视化编排与运行管线（@xyflow/react） |
| `/tasks` | TasksPage | 任务 | `ListTodo` | 任务队列与执行进度 |
| `/models` | ModelsPage | 模型 | `Database` | 模型下载 / 导入 / 管理 |
| `/settings` | SettingsPage | 设置 | `Settings` | 服务器 / 计算 / 模型 / 界面配置 |
| `*` | NotFoundPage | —（不在导航） | — | 404 兜底页 |

当前所有业务页面为占位实现（`Placeholder`），后续迭代按本规范填充。

---

## 8. 目录结构

```
frontend/src/
├── api/
│   ├── types.ts        # 全部 API / WS 类型定义
│   ├── client.ts       # REST 客户端（api.* 方法）
│   └── ws.ts           # WebSocket 管理器（自动重连 + 状态跟踪）
├── components/
│   ├── layout/
│   │   ├── header.tsx       # 顶栏：应用名 + WS 指示 + 主题切换
│   │   ├── sidebar.tsx      # 左侧导航
│   │   └── page-container.tsx  # 页面容器（标题 + 操作区 + 内容）
│   ├── ui/             # shadcn/ui 组件
│   └── placeholder.tsx # 占位内容
├── hooks/
│   └── use-ws-state.ts # WS 连接状态订阅 hook
├── lib/
│   ├── utils.ts        # cn() + formatUptime/formatBytes/formatMB
│   └── constants.ts    # CATEGORY_LABELS + STATUS_COLORS
├── pages/              # 路由页面
├── store/
│   └── theme.ts        # 主题状态（zustand，默认深色，持久化）
├── App.tsx             # 布局 + 路由
├── main.tsx            # 入口（BrowserRouter）
└── index.css           # Tailwind 入口 + 主题变量
```

---

## 9. 实现约定

- 所有导入使用 `@/` 路径别名（映射到 `src/`）。
- 所有界面文本使用简体中文。
- 颜色一律使用语义令牌（`bg-card`、`text-muted-foreground`、`text-status-running`），禁止硬编码十六进制色值。
- 类名合并使用 `cn()`（`clsx` + `tailwind-merge`）。
- 格式化辅助函数统一来自 `src/lib/utils.ts`：
  - `formatUptime(secs)` → `1h 1m 1s`
  - `formatBytes(bytes)` → `1.5 MB`
  - `formatMB(mb)` → `2.0 GB`
