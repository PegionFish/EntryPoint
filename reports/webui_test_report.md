# WebUI 端到端测试报告

> 日期：2026-07-29 | 测试人：Agent K (自动化)

## 环境

| 项目 | 值 |
|---|---|
| OS | RHEL 9.8, kernel 7.1.1 |
| Rust | 1.97.1 |
| Node.js | v20.20.2 |
| GPU | Tesla P4 8GB, CUDA 13.0 |
| ffmpeg | 5.1.10 |

## 构建验证

| 检查项 | 结果 |
|---|---|
| `cargo test` | ✅ 134 passed, 0 failed |
| `cargo clippy` | ✅ 0 warnings |
| `cargo build --release` | ✅ 3m 34s |
| `npm run build` | ✅ 2087 modules, 828ms |
| TypeScript `tsc --noEmit` | ✅ 0 errors |

## API 端点测试

| 方法 | 路径 | 状态码 | 结果 |
|---|---|---|---|
| GET | `/api/health` | 200 | ✅ `{"status":"ok","version":"0.1.0"}` |
| GET | `/api/devices` | 200 | ✅ 2 设备 (Tesla P4 + CPU) |
| GET | `/api/modules` | 200 | ✅ 5 模块全部发现 |
| GET | `/api/modules/:id/status` | 200 | ✅ 返回 stopped 状态 |
| GET | `/api/modules/:id/logs` | 200 | ✅ 返回空日志 |
| GET | `/api/config` | 200 | ✅ 含 server 段 (allow_public=false) |
| PUT | `/api/config` | 200 | ✅ 配置往返一致 |
| GET | `/api/models` | 200 | ✅ 5 模块模型列表 |
| GET | `/api/deps` | 200 | ✅ ffmpeg 可用 |
| GET | `/modules/:id` (SPA) | 200 | ✅ 返回 index.html |

## 页面组件验证

| 页面 | 文件 | 大小 | 状态 |
|---|---|---|---|
| 仪表盘 | dashboard.tsx | 15.3KB | ✅ |
| 模块管理 | modules.tsx | 8.8KB | ✅ |
| 模块详情 | module-detail.tsx | 15.4KB | ✅ |
| 管线编辑器 | pipeline.tsx | React Flow | ✅ |
| 任务中心 | tasks.tsx | 12.8KB | ✅ |
| 模型管理 | models.tsx | 15.9KB | ✅ |
| 设置 | settings.tsx | 19.8KB | ✅ |

## 共享组件

status-badge, confirm-dialog, empty-state, loading-skeleton, device-card, module-card, log-viewer — 全部创建并通过 TS 检查。

## 已知限制

1. WebSocket 日志流未接通（process.rs stdout/stderr 未管道到 broadcast channel）
2. Pipeline API 为占位（前端 UI 已完成，API 调用预留）
3. 模块设备映射无 API 端点（详情页显示 "—"）
4. JS bundle 667KB > 500KB（建议后续添加路由级代码分割）
