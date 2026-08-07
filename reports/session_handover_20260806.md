# Session 交接报告（2026-08-06）

主分支 master HEAD = `ab28afa`，已推送 origin（与远端同步）。统合门禁全绿：clippy 零警告、workspace 测试 **1085 passed / 0 failed**、前端 build 通过。

## 本 session 已完成（commit 均已在 origin/master）

| Commit | 内容 |
|---|---|
| f8e94c2 | 防火墙根治：ep-core process.rs 注入 EP_HOST=127.0.0.1，deep-filter/qwen3-tts/faster-whisper adapter 硬编码 0.0.0.0 改读 EP_HOST |
| 87b8c8b | HETERO-PY 遗作收尾：D-1/D-2/D-5/D-11 adapter 设备消费修复 + torch 2.11.0+cu130 依赖 + EP_MODEL_ID/U2NET_HOME + model.rs modelscope_endpoint 接线 + Cargo.toml exclude runtime |
| 66b087a | GAP-2：ep-daemon handler 补测 +14（deps/devices/health/ws/modules/execute 错误路径），测试端口区间迁至 48xxx 消除与生产 18000-19000 的并发误判 flake |
| ad6cea1 | [python] 配置裸命令名改走 PATH 解析（消除 daemon uv/python 路径误告警） |
| 3370d6c | 模型下载 tar.gz 解压后展平嵌套包装目录（df3 tmp/export 问题） |
| 6502400 | uv 安装健壮性：--index-strategy unsafe-best-match + 半壳 venv 自动拆除 |
| 480ec53 | deep-filter 推理修复：enhance() 移除不支持的 min_db，改为 STFT 增益下限后处理（CUDA 实测降噪 -15.4→-51.4dB） |
| f485da7/0c73c24/c951a9d/c233b0f | SCHED-WIRE（worktree 续做后 rebase ff 合并）：D-3/D-6 + D-4 ep-core assign_module_device 下沉 + daemon 三处启动路径接线调度器（+22 测试） |
| a1316e9 | ep-desktop 设备选择切换到 ep-core 共享调度器 + 消费 disabled_backends（桌面私有实现去重） |
| d0447e9 | venv 就绪门禁：哈希判定（is_venv_ready）统一手动/自动启动路径，半壳 venv 回归测试 |
| 0c1a101 | artifacts API 修复（served 副本优先）+ /api/deps 仅对声明 torch 的模块输出 torch_cuda |
| 7d396a1 | 仓库卫生：停止跟踪 __pycache__/*.pyc + .gitignore 补全 |
| ab28afa | WebUI 浏览器实机验证截图留证（reports/webui-verify-*.png） |

## 实机验证结论

- **WebUI（Browser 实测，全过）**：仪表盘/模块启停（rembg 实时状态+端口）/任务+产物下载/pipeline 画布/设置页，522 请求全 200、console 零报错、WS 实时推送正常。轻微项：无独立 /devices 路由（设备聚合在仪表盘）；仪表盘模块表"设备"列恒显"暂不支持"与卡片不一致；模块卡片不显示端口。
- **桌面 GUI（ComputerUse 实测）**：启动/渲染/导航/模块列表通过；桌面端不连 daemon、自带 ep-core 后台 loop。注意：实测用的是 8/5 陈旧二进制（7 项导航），源码已有导航收敛（5 项）+模块页卡片化，**下轮 UI 工作务必先重建二进制再看现状**。
- **E2E**：daemon 冷启动→模块拉起→video_to_srt 全链路 completed；deep-filter venv 干净重装 16s；全程仅 127.0.0.1 绑定。

## 下次 session 待办

1. **桌面 GUI 向 WebUI 设计最小化同步**（用户已裁决，本 session 已取消未实施）：以 reports/webui-verify-*.png 为基准，最小改动对齐 egui 主题/布局/卡片呈现；顺手可修"python 探测失败弹系统错误窗（0xc0000142）→静默降级"。
2. 桌面端偶发**自发无声退出**待非沙箱环境复现排查（stderr 无 panic、WER 无记录）。
3. 遗留小项：modules.rs start_module invalidManifest 500 死代码；check_health TOCTOU 与 localhost IPv6 解析提示；default_backend 死配置待产品决策；qwen3-tts 默认变体指向 1.7B 而本地为 0.6b（配置现状非缺陷）。

## 环境现状

- 5 个 venv 全建成（deep-filter/qwen3-tts 为 torch 2.11.0+cu130，torch.cuda 可用）；models/deep-filter-df3 已补 checkpoints/model_120.ckpt.best（Python API 需要）。
- config/app.toml 本地未提交改动（host=127.0.0.1 + 代理）为刻意保留；.desktop-verify/ 未跟踪可清理。
- 验证用 daemon（曾 PID 11352，端口 9800）若仍在运行可直接停止。
- 防火墙有 4 条历史 python 入站规则（早期允许弹窗留下），可酌情清理；现行代码已全回环绑定不再触发。
