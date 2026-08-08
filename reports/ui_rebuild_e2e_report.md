# 统一 UI 重构 E2E 验收报告（双端）

日期：2026-08-09 | 任务：Task #37（重构后全量 E2E 验收收尾）
验收对象：WebUI（Chrome 浏览器实测）与桌面端（entrypoint.exe，UIA 无障碍树验证）
结论速览：**WebUI 全部通过；桌面端功能项通过、纯视觉项被 Parsec 虚拟显示环境阻塞（非代码回归）**。

---

## 1. 重构范围回顾（W0–W3 波次）

依据 `docs/UNIFIED_UI_REDESIGN_PROPOSAL.md`（深空仪表盘设计语言），双端按四波次推进，commit 链如下：

| 波次 | Commit | 内容 |
|------|--------|------|
| 方案入库 | `5809460` | docs：统一 UI 重设计方案（设计语言 / 波次计划） |
| W0 | `562feac` | webui：统一设计语言令牌基线与基础组件样式 |
| W0 | `d87302b` | desktop：落地深空仪表盘令牌基线 |
| W1 | `6231165` | webui：仪表盘与设置页样式重构（§6.2-D/§7.1/§7.5） |
| W1 | `d18f587` | desktop：设置页与仪表盘重设计（§7.1/§7.5/§9/§13） |
| W2 | `86b2fb8` | webui：模块页与任务页按统一方案重构 |
| W2 | `fe70b59` | desktop：模块页与任务页重设计（§7.2/§7.4） |
| W3 | `8c6b0e5` | webui：管线编辑器深空仪表盘样式层改造（仅样式层，能力零改动） |
| W3 | `cbbec05` | desktop：管线编辑器两态重做（§7.3） |

覆盖范围：仪表盘、模块页、管线页（库视图 + 编辑器）、任务页、设置页五页 + 主题切换 + 窗口自适应断点。

---

## 2. WebUI 验收结果（执行人：Emily）—— 全部通过

### 2.1 总体结论
- 五个页面 + 响应式断点验收 **全部通过**；
- 浏览器 console **零错误**；API 请求 **全部 200**；
- 管线编辑器 **行为零回归**（W3 仅样式层改造，加载/编辑/连线/参数/运行对话框能力与重构前一致）。

### 2.2 证据（截图，位于 `runtime/`，不入库）
| # | 文件 | 覆盖 |
|---|------|------|
| 01 | `runtime/webui-accept-01-dashboard.png` | 仪表盘（设备/模块/任务统计） |
| 02 | `runtime/webui-accept-02-modules.png` | 模块页列表 |
| 03 | `runtime/webui-accept-03-module-detail-drawer.png` | 模块详情抽屉 |
| 04 | `runtime/webui-accept-04-pack-drawer.png` | 包详情抽屉 |
| 05 | `runtime/webui-accept-05-direct-run-drawer.png` | 直接运行抽屉 |
| 06 | `runtime/webui-accept-06-pipeline-editor.png` | 管线编辑器 |
| 07 | `runtime/webui-accept-07-pipeline-library.png` | 管线库 |
| 08 | `runtime/webui-accept-08-video-to-srt-canvas.png` | 视频转字幕画布 |
| 09 | `runtime/webui-accept-09-node-param-panel.png` | 节点参数面板 |
| 10 | `runtime/webui-accept-10-pipeline-run-dialog.png` | 管线运行对话框 |
| 11 | `runtime/webui-accept-11-tasks.png` | 任务列表 |
| 12 | `runtime/webui-accept-12-task-expanded.png` | 任务展开详情（产物） |
| 13 | `runtime/webui-accept-13-settings.png` | 设置页（中文） |
| 14 | `runtime/webui-accept-14-settings-english.png` | 设置页（英文） |
| 15 | `runtime/webui-accept-15-responsive-1000px.png` | 响应式 1000px |
| 16 | `runtime/webui-accept-16-responsive-800px.png` | 响应式 800px |
| 17 | `runtime/webui-accept-17-responsive-drawer.png` | 响应式下的抽屉 |

### 2.3 低级别发现（不阻塞）
- **D1**：设置页实际为 9 个区块，与验收矩阵记载的「十区块」不符。核查源码确认为 **9 Section**，判定为矩阵口径笔误，**无需改码**。
- **D2**：设计文档 §3.1 定义的渐变效果有 6 处未落地齐全（侧栏激活项缺 2px 青→靛竖条、Logo 字标无渐变；`.text-gradient` 类已定义但未被引用）。属打磨项。
- **D3**：语言切换后的 toast 提示使用 **切换前** 的语言渲染，纯观感问题。

---

## 3. 桌面端验收结果（执行人：Ivan）—— 功能通过，视觉项环境阻塞

完整原始材料：`runtime/ui-verify/e2e-final/REPORT.md`（被测构建：2026-08-09 03:53 release 版）。

### 3.1 环境阻塞（如实记录）
主显示输出为 **Parsec Virtual Display Adapter**（Win32_DesktopMonitor 无活跃分辨率，实际 GPU 为 Intel + RTX 5090 D）。应用启动后 **整窗白屏**（像素统计白色占比 87%，多次尝试完全一致）。已按预案穷尽排查：
1. 重启应用 2 次；
2. Win+Ctrl+Shift+B 显卡驱动重置；
3. `WGPU_BACKEND=gl` 与 `vulkan` 分别启动。

以上全部仍白屏；**干净旧版本同样复现，确认为环境渲染问题、非本次代码回归**。视觉验收被阻塞，转为 **UIA 无障碍树** 验证应用状态与交互。另注：会话内合成键盘/鼠标输入（SendKeys、keybd_event、mouse_event、WM_CHAR）均无法送达，仅 UIA Invoke/Toggle/RangeValue 语义动作可用，故文本输入与画布手势类项目无法实测。

### 3.2 UIA 验证逐项结论（9 大项）
| # | 项目 | 结论 |
|---|------|------|
| 1 | 首帧与窗口 | 功能通过：6s 出窗口（<10s 达标），默认 1571x996，最大化→3862x2110、还原、Resize 1200x800 均成功且内容完整；「无裁切/无黑边」视觉项阻塞 |
| 2 | 整体视觉（深空配色/玻璃卡片/四态色/渐变） | 环境阻塞 |
| 3 | 仪表盘 | 通过：设备 4 / 模块 5 / 运行中 0 / 错误 0；设备卡含 RTX 5090 D（cuda，8982/32607MB·38°C）、Intel AI Boost、Intel Graphics（openvino）、Core Ultra 7 270K Plus（cpu）；模块概览表 5 行；依赖检测（ffmpeg 可用、deep-filter 可用 torch 2.11.0+cu130、5 项 torch 缺失带安装指引） |
| 4 | 模块页 | 部分通过：5 张模块卡信息齐全；直跑抽屉通过（能力 ComboBox、attenuation=100、min_db=-60、浏览+输入框、提交执行按钮）；**搜索过滤环境阻塞**（无法注入文本）；卡片辉光/徽章视觉阻塞 |
| 5 | 管线页（W3） | 通过：库视图「音频提取 3 节点」「视频转字幕 4 节点」卡片就绪；编辑器工具栏/节点面板/管线属性/VRAM 账本/验证均正常；**dirty 拦截通过**（添加 llm 节点→返回弹「未保存的改动」，取消留在编辑器、丢弃并返回后库内仍 4 节点未污染）；滚轮缩放/MiniMap/框选/Delete 环境阻塞 |
| 6 | 任务页 | 通过：SegmentedTabs 全部 33 / 运行中 0 / 已完成 16 / 失败 16 / 已取消 1，计数正确；切换过滤精确生效，空态文案正常 |
| 7 | 设置页 | 通过：三开关复验通过（启动时检查更新、允许显存超额、保留工作区，旧 bug 未复现）；端口 Spinner 18000↔18001 可编辑还原；保存写入 `config/app.toml`（diff 三值一致）；重启后持久化保持；双列卡片视觉阻塞 |
| 8 | 主题切换 | 通过：深色→浅色→深色往返即时切换，全程无冻结；实际配色视觉阻塞 |
| 9 | 关闭应用 | 通过：侧栏「退出」后进程干净退出 |

### 3.3 汇总
UIA 层面：**9 大项中 6 项完全通过**（首帧/窗口、仪表盘、管线、任务、设置、主题、退出），模块页除搜索输入外通过；**零崩溃、零冻结、零控件丢失**。
待真实显示器恢复后仅需补验：纯视觉项（配色/玻璃卡片/渐变）、搜索过滤、画布手势（缩放/框选/删除/MiniMap）。

---

## 4. 合并缺陷清单与优先级

| 优先级 | 类别 | 缺陷 | 处置建议 |
|--------|------|------|----------|
| P2 | 环境/渲染 | Parsec 虚拟显示器下 wgpu 整窗白屏（重启/驱动重置/gl/vulkan 均无效，干净旧版同症，**非代码回归**） | 排查 wgpu 在虚拟显示适配器上的 surface/present 兼容性；评估 WARP/软渲染后备通道 |
| P3 | 无障碍/输入 | egui 文本框不响应 UIA `ValuePattern.SetValue`（搜索框、管线属性输入框无法由自动化工具键入） | 跟进 egui accesskit 文本输入支持 |
| P3 | 无障碍/可用性 | `TogglePattern.Toggle` 后回读状态为旧值，需等下一帧（约 >1s），自动化易误判切换失败 | 状态同步时序跟进 |
| P3 | 无障碍/语义 | 管线卡片容器、画布节点暴露为无名 Custom 控件 | 补充 AutomationName |
| P3 | WebUI 打磨 | D2：设计 §3.1 渐变 6 处未齐（侧栏激活竖条、Logo 字标渐变、`.text-gradient` 未引用） | 样式补齐 |
| P3 | WebUI 打磨 | D3：语言切换 toast 使用切换前语言 | 观感修复 |
| — | 记录 | D1：设置页 9 区块 vs 矩阵「十区块」为口径笔误 | 已判定无需改码 |

---

## 5. 结论与下次会话待办

### 结论
本次双端统一 UI 重构（W0–W3）在可验证维度上 **全部达标**：WebUI 五页 + 响应式全量通过、管线编辑器零回归；桌面端功能与交互在 UIA 层面全部通过。桌面端纯视觉验收受 Parsec 虚拟显示环境阻塞，已穷尽预案并留证，阻塞项与代码无关。

### 下次会话待办
1. **真实显示器环境补验桌面端视觉项**（深空配色/玻璃卡片/四态色/渐变）、模块搜索过滤、画布手势（缩放/框选/删除/MiniMap）；
2. **WebUI D2 渐变补齐**（侧栏激活 2px 青→靛竖条、Logo 字标渐变、`.text-gradient` 引用落地）与 D3 toast 语言修复；
3. **无障碍三项**：SetValue 文本注入、Toggle 状态回读时序、管线卡/画布节点 AutomationName；
4. **DPI 振荡问题跟进**（历史遗留项，与本环境虚拟显示相关，待真实显示器复测）。
