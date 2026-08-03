# Wave 4 端到端测试报告 — WebUI 端到端可用性与模型管理强化

> 日期：2026-08-03 ~ 2026-08-04 | 测试方：4 并行门禁/E2E 代理 + 门禁修复
> 配套记录：PROGRESS.md「WebUI 端到端可用性与模型管理强化」章节

## 环境

| 项目 | 值 |
|---|---|
| OS | RHEL 9 系（kernel 7.1.1-1.el9.elrepo.x86_64，glibc 2.34） |
| Rust | 1.97.1 |
| Node.js | v20.20.2 |
| GPU | Tesla P4（CUDA 路径不可用 → 设备级回退 CPU） |
| ffmpeg | 5.1.10 |
| HTTP 代理 | 127.0.0.1:20171（HuggingFace 等外网访问） |
| 浏览器 | Playwright Chromium headless（手工补齐 6 个系统库） |

## 测试矩阵总览

| # | 测试套 | 范围 | 结果 |
|---|---|---|---|
| 1 | API 冒烟 | 40 项端点/行为断言 | ✅ 40/40（唯一偏差 D1 已修） |
| 2 | 浏览器巡测 | 8 页面真实渲染 + 控制台错误 | ✅ 零控制台错误（修复 2 处前端缺陷） |
| 3 | 管线全流程 E2E | video_to_srt 真实媒体 | ✅ 通过（task-20260803-184815-0000） |
| 4 | 模型回环 | 删除/文件夹上传/zip 上传/URL 下载 | ✅ 全通 |
| 5 | 全新安装路径 | df3 自动 venv + 下载 | ✅ 通过（约 16 分钟） |
| 6 | HF 缓存回收 | cleanup_hf_cache | ✅ 8.7G → 2.9G（-5.8G） |
| 7 | 最终门禁 | cargo test + clippy | ✅ 288 测试全过、零警告 |

## 1. API 冒烟（40 项）

- 覆盖：health / devices / modules / config（含落盘往返）/ models（download/delete/check-update/downloads/upload）/ pipelines（CRUD/builtin 保护）/ tasks（执行/查询/产物 302）/ ws 聚合端点 / SPA fallback。
- 结果：40 项通过。
- 唯一偏差 D1：模块详情 / import 相关 404 响应缺中文错误消息 —— 已修复后复测通过。

## 2. 真实浏览器巡测（Playwright Chromium headless）

- 环境补齐：容器缺 6 个 Chromium 依赖系统库，手工安装后启动。
- 巡测 8 个页面（仪表盘 / 模块管理 / 模块详情 / 管线编辑器 / 任务中心 / 模型管理 / 设置 / SPA 深链 fallback），逐页检查渲染与控制台错误。
- 结果：零控制台错误。
- 巡测中发现并修复：
  1. 内置管线边不渲染 —— React Flow 端口名与 TOML 桥接后名称不一致，端口名归一后修复；
  2. 主题下拉与生效主题不同步 —— 状态回填修复。

## 3. 管线全流程 E2E（video_to_srt，真实媒体）

- 管线：内置 `video_to_srt`（extract-audio[ffmpeg] → asr[faster-whisper] → output[file_output]）
- 输入：15s 中文解说视频 `w4c-input.mp4`（1,879,053 B）
- 任务：**task-20260803-184815-0000**，总耗时 **85s**
- 执行路径：ffmpeg 提取 WAV → ASR large-v3（CUDA→CPU 设备级回退）→ SRT 导出 → 产物经 302 下载
- 产物（磁盘复核，workspace/tasks/task-20260803-184815-0000/）：

| 产物 | 字节数 | 说明 |
|---|---|---|
| extract-audio_output.wav | 480,082 | ffmpeg 节点输出 |
| asr_output.srt | 455 | ASR 节点输出（8 条字幕） |
| output_output.srt | 455 | file_output 最终产物（extension 派生路径生效） |

- SRT 内容摘录（真实中文转写，非占位）：

```
1
00:00:00,140 --> 00:00:02,100
这是美军现役最大的直升机

2
00:00:02,100 --> 00:00:03,860
CH-53E超级种马
```

- WS 进度：任务节点状态经 /ws 聚合端点实时推送至前端任务页/管线页。

## 4. 模型回环

| 步骤 | 模型 | 结果 |
|---|---|---|
| 删除 | rembg-u2net | ✅ 目录清除 |
| 文件夹上传（multipart 多文件） | rembg-u2net | ✅ |
| 再次删除 | rembg-u2net | ✅ |
| zip 上传（归档剥层 + zip-slip 防御） | rembg-u2net | ✅ meta 写入（见下） |
| URL 下载（经代理） | rembg-isnet | ✅ 178MB，进度采样完整、meta 写入 |
| 删除/下载防重 | — | ✅ 409/412 语义正确 |

- 磁盘复核 `.ep_meta.json`：
  - `models/rembg-u2net/.ep_meta.json`：`source = "local_import"`，`total_size_bytes = 175997641`，`downloaded_at = 2026-08-03T17:54:44Z`
  - `models/deep-filter-df3/.ep_meta.json`：`source = "url"`，repo = `https://huggingface.co/Serkan007/DeepFilterNet3-ONNX/resolve/main/DeepFilterNet3_onnx.tar.gz`，`total_size_bytes = 8589309`，`downloaded_at = 2026-08-03T18:37:38Z`

## 5. 全新安装路径（df3）

- 场景：deep-filter venv 不存在时触发下载的 412 死锁 → 改为自动 venv 准备。
- 结果：自动创建 venv（含 torch）约 16 分钟 + 模型下载 15s，全链路无人工干预完成。
- 原 df3 URL 死链已切换为 HF 镜像 Serkan007/DeepFilterNet3-ONNX，下载内容已验证。

## 6. HF 缓存回收

- `cleanup_hf_cache` 对 faster-whisper-large-v3 执行：HF 缓存占用 **8.7G → 2.9G**（回收 5.8G，消除 3 倍膨胀）。

## 7. E2E 途中修复的产品缺陷清单

| # | 缺陷 | 影响 | 修复 |
|---|---|---|---|
| 1 | ffmpeg 节点 {input}/{output} 占位符与实际参数失配 | 两条内置管线必挂 | 占位符协议对齐 |
| 2 | output_extension 未被尊重 | 产物扩展名错误 | 按声明扩展名输出 |
| 3 | faster-whisper CUDA 不可用时无回退 | Tesla P4 上 ASR 必败 | 设备级 CUDA→CPU 回退 |
| 4 | ASR 无 SRT 导出协议 | 字幕管线断链 | output_format/output_path 模块产物协议 |
| 5 | file_output 不派生扩展名路径 | 最终产物路径错误 | extension 派生 |
| 6 | venv 缺失时下载 412 死锁 | 全新安装不可用 | 自动 venv 准备 |
| 7 | df3 下载 URL 死链 | deep-filter 模型不可得 | HF 镜像（内容已验证） |
| 8 | default_source 死配置 | 选源无效 | 接线生效 |
| 9 | 内置管线边不渲染 / 主题下拉不同步 | 前端可见缺陷 | 端口名归一 / 状态回填 |
| 10 | 模块详情/import 404 无中文消息（冒烟 D1） | 错误提示不友好 | 中文错误消息 |

## 8. 最终门禁

| 检查项 | 结果 |
|---|---|
| cargo test | ✅ 288 passed, 0 failed |
| cargo clippy | ✅ 0 warnings |
| 桌面端 | 编译 + 单测 + Windows 交叉编译通过（无头环境无法运行时验证） |

## 已知限制

1. 首次下载自动 venv 准备（含 torch）约 15-20 分钟，超常见客户端超时 —— 重试即成功，后续考虑异步化 + 任务化。
2. daemon 重启不回收模块子进程：重启前需先 stop 模块，否则端口占用。
3. deep-filter 模块启动健康检查 30s 超时（torch+CUDA 首次导入慢，待查）。
4. max_concurrent_downloads 配置项保留未实现；任务工作目录（workspace/tasks/*）无自动清理。
5. 桌面端 GUI 无头环境无法运行时验证（仅编译 + 单测 + 交叉编译覆盖）。

## 结论

WebUI 已完成从"页面可见"到"端到端真实可用"的跨越：WS / 日志 / 管线执行 / 模型全生命周期（下载/上传/删除/更新/回环）全部接通，真实浏览器 8 页面零错误，真实媒体 video_to_srt 全流程产出可验证的中文转写 SRT，全新安装路径与缓存治理验证通过，288 测试 + clippy 零警告收官。遗留项集中于长耗时 venv 准备的体验优化与进程/工作目录生命周期治理，均不阻塞核心功能使用。
