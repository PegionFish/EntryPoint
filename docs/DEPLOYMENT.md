# EntryPoint 部署指南（Linux）

本文档描述 EntryPoint 服务器版（ep-daemon + WebUI）的 Linux 部署。
交付模型：**解压目录自包含**——ZIP 解压到任意目录，一切运行在该目录内，
不复制到 `/opt`、不绑定发行版目录布局。

> Windows 部署（start-daemon.bat）见 README「Windows 快速开始」。

---

## 1. 交付物

`./build.sh server` 产出（`dist/` 下）：

| 产物 | 定位 |
|---|---|
| `EntryPoint-vX-linux-<arch>-server.zip` | **主交付物**：通用自包含包，含交互式 `deploy.sh` |
| `...server.tar.gz` | 兜底（无 zip 工具环境） |
| `entrypoint-server_*.deb` / `*.rpm` | 原生包管理集成（FHS 布局，适合批量/镜像） |
| `arch-server/PKGBUILD` + 源包 | Arch `makepkg` 原生构建 |

**ZIP 内容**：

```
EntryPoint-vX-linux-x86_64-server/
├── bin/ep-daemon          # daemon 二进制
├── bin/ep-pack            # 整合包 CLI
├── deploy.sh              # ★ 交互式部署/配置脚本（见 §3）
├── start-daemon.sh        # 前台调试启动
├── entrypoint.service     # systemd unit 参考模板（deploy.sh 会现场渲染，此文件供高级用户手装）
├── webui/                 # WebUI 静态资源
├── config/                # app.toml + 内置管线 + constraints.txt
├── modules/               # Python 模块适配器（不含模型权重）
├── workspace/             # 运行期工作目录
├── VERSION.txt
└── README.md
```

**模型权重不入包**（体积原因）：首次部署后通过 WebUI 模块页下载 / 浏览器上传 /
本地路径导入；已有权重目录可直接放入 `<EP_ROOT>/models/`（见 §7）。

---

## 2. 快速部署（三步）

支持发行版族：**Debian/Ubuntu、Fedora、RHEL/CentOS/Rocky/Alma、Arch/Manjaro**
（自动探测；未知发行版跳过依赖安装并警告）。

```bash
unzip EntryPoint-vX-linux-x86_64-server.zip
cd EntryPoint-vX-linux-x86_64-server
./deploy.sh install          # 交互式；或 ./deploy.sh install --yes 全取缺省
```

完成后浏览器访问 `http://127.0.0.1:9800`（端口以配置为准）。

install 流程（每步可交互覆盖，`--yes` 取缺省）：

1. **发行版族探测** → deb / rpm / arch / unknown
2. **系统依赖**：ffmpeg、python3、curl（幂等探测，已装跳过）+ **uv**
3. **配置向导**：host / port / allow_public / API token / 代理（合并式写入
   `config/app.toml`，保留其余内容与用户既有值）
4. **systemd 服务**（可选，缺省注册）：unit 现场渲染（见 §5），启动后
   轮询 `/api/health` 自检；失败打印 `journalctl -n 50` 指引
5. **防火墙**：host 为回环地址时自动跳过；否则 firewalld → ufw 顺序探测放行
6. **SELinux**（rpm 族且 Enforcing）：`semanage port` 添加 `http_port_t` 标签

---

## 3. deploy.sh 命令参考

| 子命令 | 作用 |
|---|---|
| `install` | 完整安装/升级（幂等：重跑即升级，保留 config/models/runtime/workspace） |
| `uninstall` | 软卸载：停服务 + 删 unit，**保留数据**；`--purge` 删除整个部署目录 |
| `status` | 服务状态 + 健康探测 + 版本 + 依赖体检 |
| `start` / `stop` | systemctl 封装（无 unit 时提示前台方式） |
| `logs [-f] [-n N]` | journalctl 封装 |
| `configure` | 单独重跑配置向导 |
| `check` | 只读诊断（依赖/端口/权限/unit 一致性），退出码 0/1 |

常用 flags：

```text
--yes                    非交互，全部取缺省
--host / --port          监听地址/端口
--allow-public           允许公网访问（host 非回环时建议配合 --api-token）
--api-token <s> / --no-token
--with-service / --no-service    是否注册 systemd 服务
--user <name>            服务用户（缺省 = 部署目录属主）
--skip-deps              跳过系统依赖安装
--distro <family>        强制发行版族（deb|rpm|arch）
--ffmpeg-source fusion|free      rpm 族 ffmpeg 来源（见 §4）
--no-firewall / --skip-selinux
--purge                  配合 uninstall：删除部署目录
```

---

## 4. 发行版依赖矩阵

| 依赖 | Debian/Ubuntu | Fedora | RHEL 系 | Arch |
|---|---|---|---|---|
| ffmpeg | `apt-get install ffmpeg` | RPM Fusion free | RPM Fusion free（或官方 `ffmpeg-free` 兜底） | `pacman -S ffmpeg` |
| python3 | `python3` | `python3` | `python3` | `python` |
| uv | astral installer（curl） | 同左 | 同左 | `pacman -S uv` |
| curl | `curl` | `curl` | `curl` | `curl` |

**RHEL/Fedora 的 ffmpeg 来源**：EPEL 与官方仓不提供完整 ffmpeg
（Fedora 许可政策）；官方 `ffmpeg-free` 编解码受限（缺部分专利编码器）。
deploy.sh 缺省经 **RPM Fusion free** 安装完整 ffmpeg（video_to_srt 等管线
需要完整编解码）；离线或拒绝第三方仓时用 `--ffmpeg-source free` 改装
`ffmpeg-free` 并接受功能受限。

**Python 版本**：模块 venv 要求 `>=3.10,<3.13`；系统 Python 过新（如 Arch 3.14）
时 uv 自动下载托管 CPython 至 `runtime/uv-python/`（自包含，见 §6），无需手工干预
（离线环境预置：`uv python install 3.12`）。

---

## 5. systemd 服务

deploy.sh 渲染的 unit（`/etc/systemd/system/entrypoint.service`）要点：

```ini
[Service]
User=<部署目录属主>                      # --user 可覆盖；root 属主会警告
Environment=EP_ROOT=<部署目录绝对路径>    # daemon 根目录解析的唯一权威来源
WorkingDirectory=<部署目录>
ExecStart=<部署目录>/bin/ep-daemon
Restart=on-failure
TimeoutStopSec=30                        # SIGTERM 优雅回收窗口（逐模块停止+释放端口）
# 安全加固（不启用 ProtectHome：部署目录可能在 /home 或任意挂载点下）
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=<部署目录>
PrivateTmp=yes
UMask=0027
```

**按项目约定 deploy.sh 绝不执行 `systemctl enable`**（开机自启由用户显式决定）：

```bash
sudo systemctl enable entrypoint     # 需要自启时手动执行
```

优雅退出语义：`systemctl stop/restart` 发 SIGTERM → daemon 逐个停止运行中的
模块子进程（进程组级 SIGTERM + 5s 宽限 + SIGKILL 兜底）并释放端口后退出。
`kill -9` 强杀不走该路径，可能留下孤儿子进程。

---

## 6. 运行时目录（自包含布局）

所有运行期数据都在部署目录内：

```
<EP_ROOT>/
├── config/app.toml          # 主配置（配置向导合并式写入）
├── models/                  # 模型权重缓存（下载/上传/导入落盘处）
├── runtime/
│   ├── venvs/<module>/      # 模块虚拟环境（uv 创建，跨平台损坏自动重建）
│   ├── uv-python/           # uv 托管 CPython（系统 Python 不满足约束时自动下载）
│   ├── .uv-cache/           # uv 包缓存（UV_CACHE_DIR 注入，不污染 ~/.cache）
│   └── tasks/               # 任务注册表与产物
└── workspace/
    ├── uploads/             # v1 推理接口输入文件约定目录（见 §8）
    └── tasks/<task_id>/     # 任务工作目录与归集产物（files/ 可下载）
```

---

## 7. 模型获取（三路径）

1. **在线下载**：WebUI 模块页选择模块变体 → 下载（HuggingFace/ModelScope/URL
   三源 + 镜像选源，WebSocket 实时进度；`[network]` 代理配置生效）
2. **浏览器上传**：文件夹多文件 / zip / tar.gz，服务端流式落盘解包
3. **本地导入**：服务器上已有权重目录直接导入；或直接把权重目录放入
   `<EP_ROOT>/models/<target_dir>/`（模块发现时按 `is_model_present` 判定就绪）

---

## 8. 输入文件约定（重要）

- **systemd 部署**：unit 带 `PrivateTmp=yes`，daemon **看不到用户的 /tmp**；
  输入文件必须位于部署目录内（建议 `workspace/uploads/`）。
- **统一推理 API（`/api/v1/inference/...`）**：`input_path` 强制要求位于
  `workspace/uploads/` 前缀内（路径安全契约，canonicalize 防穿越）。
- 管线 `file_input` 节点：路径需 daemon 可读（systemd 下同理限部署目录内）。

---

## 9. 配置参考

主配置 `config/app.toml`（完整字段见 [CONFIG_REFERENCE.md](CONFIG_REFERENCE.md)）：

```toml
[server]
host = "127.0.0.1"     # 局域网访问改 0.0.0.0（配合防火墙放行与 allow_public 评估）
port = 9800
allow_public = false   # false=IP 过滤仅放行私有/回环地址；公网暴露前务必设置 [api] token

[api]
# token = "<openssl rand -hex 32>"   # 公网/对外暴露时强烈建议配置
```

部署期配置向导覆盖最常用项；其余（compute 策略、端口范围、下载并发等）
直接编辑文件后 `./deploy.sh stop && ./deploy.sh start` 生效。

---

## 10. 防火墙与 SELinux

- **firewalld**（活动）：`firewall-cmd --permanent --add-port=<port>/tcp && firewall-cmd --reload`
- **ufw**：`ufw allow <port>/tcp`
- host 为回环地址时无需放行（deploy.sh 自动跳过并说明）
- **SELinux**（rpm 族 Enforcing）：`semanage port -a -t http_port_t -p tcp <port>`
  （缺 semanage 时装 `policycoreutils-python-utils`）；服务域保持
  `unconfined_service_t`（模块需派生 python/ffmpeg 子进程，不加 SELinuxContext）

---

## 11. 升级与卸载

- **升级**：新 ZIP 解压覆盖后重跑 `./deploy.sh install`——`bin/webui/modules`
  覆盖更新，`config/models/runtime/workspace` 保留（服务在跑会先优雅停止）
- **软卸载**：`./deploy.sh uninstall`（停服务 + 删 unit，数据全保留）
- **彻底删除**：`./deploy.sh uninstall --purge`（二次确认删整个部署目录）

---

## 12. 排障

```bash
./deploy.sh check              # 只读诊断（依赖/端口/权限/unit 一致性）
./deploy.sh logs -f            # 实时日志（= journalctl -u entrypoint -f）
./deploy.sh status             # 服务状态 + 健康探测
```

常见问题：

| 症状 | 处理 |
|---|---|
| 启动失败 `EP_ROOT` 解析异常 | 确认 unit 的 `Environment=EP_ROOT=` 指向部署目录（deploy.sh check 会核对） |
| 模块 venv 准备慢/失败 | 首次 torch 系依赖需下载 GB 级 wheel（已放宽 uv 超时到 300s）；失败会自动拆除半壳 venv，重试即可 |
| 推理报 input 不在 uploads | 输入文件移入 `workspace/uploads/`（§8） |
| systemd 下读不到输入文件 | PrivateTmp 隔离所致，输入放部署目录内（§8） |
| 端口被占 | `deploy.sh check` 报端口状态；改 `[server] port` 或释放占用 |

---

## 13. 开发者：从源码构建

前置：Rust 1.97+、Node.js 20+、uv、ffmpeg（安装方式见 README「开发环境搭建」）。

```bash
./build.sh server              # clippy + 全量测试 + release 构建 + 打包（全门禁）
./build.sh server --skip-test --skip-clippy   # 快速出包
./build.sh server -d debian-12               # 指定目标发行版（glibc 兼容性检查 + 依赖包名适配）
```

产物见 §1。源码树开发调试：`cargo run -p ep-daemon`（EP_ROOT 缺省取 cwd）。
