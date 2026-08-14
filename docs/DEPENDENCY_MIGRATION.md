# 依赖栈统一 — 机制说明与旧环境一次性迁移指南

> 对应 PACK_UNIFY_PLAN 设计 A（§3 依赖栈统一）；配置项说明见 CONFIG_REFERENCE「[python]」节。
>
> 适用读者：从"依赖栈统一"之前版本升级的部署（本机旧 venv + 全局 uv 缓存双份占用），以及想自定义/停用版本锁定的用户。

---

## 1. 机制简介

依赖栈统一由三件协同工作：

| 件 | 内容 |
|---|---|
| **应用内 uv 缓存** | 模块 venv 安装时注入 `UV_CACHE_DIR`，默认 `runtime/.uv-cache`（`[python].uv_cache_dir`）。缓存与各模块 venv（`runtime/venvs/<module>`）同盘同目录树 |
| **全局 constraints** | `config/constraints.txt`（`[python].constraints` 指向），对每个模块的 `uv pip install` 追加 `-c`。锁定 torch 全家桶（torch/torchvision/torchaudio == 2.13.0，对齐 cu130 基线），保证 deep-filter、qwen3-tts 等多模块解析到**同一版本** |
| **硬链接去重** | 安装显式 `--link-mode hardlink`。缓存是唯一的物理拷贝来源，各 venv 中的同版本包文件是硬链接薄壳，跨模块只占一份物理磁盘 |

配套的**哈希联动**（P2-18）：每个模块 venv 目录下的 `.ep_deps_hash` 输入 = requirements.txt 字节 + constraints 文件字节 + link-mode 标记。任一变化（含 constraints 从无到有）都会使哈希失配 → 下次启动该模块时自动重装依赖。

## 2. 旧环境一次性迁移步骤（推荐执行）

**为什么迁移**：旧版本用全局 uv 缓存 + 无 constraints 的 venv。升级后哈希口径变化本就会触发各模块自动重装，但旧的 venv 内容与全局缓存仍留在磁盘上形成双份占用；做一次清理可立即回收空间并避免旧残留混入。重装属预期行为，不是故障。

**步骤**（先停止 daemon 与所有运行中模块）：

1. **删除各模块旧 venv**（含其中的 `.ep_deps_hash`）：

   ```powershell
   # Windows（应用根目录下）
   Remove-Item -Recurse -Force .\runtime\venvs\*
   ```

   ```bash
   # Linux（应用根目录下）
   rm -rf runtime/venvs/*
   ```

2. **删除旧全局 uv 缓存**（新缓存将重建在 `runtime/.uv-cache`）：

   ```powershell
   # Windows
   Remove-Item -Recurse -Force "$env:LOCALAPPDATA\uv\cache"
   ```

   ```bash
   # Linux
   rm -rf "${XDG_CACHE_HOME:-$HOME/.cache}/uv"
   ```

   > 注意：若该机器的全局 uv 缓存还服务于 EntryPoint 以外的 Python 项目，删除后那些项目下次安装也会重新下载——uv 缓存永远可安全重建，只是首次会慢。

3. （可选）若 `runtime/.uv-cache` 已部分存在且想完全干净：同样可整目录删除，会自动重建。

4. **重新启动应用**。首次启动各 torch 模块时自动重建 venv 并安装依赖：
   - 第一个 torch 模块需完整下载 torch 全家桶（约 4G，视网络 10~20 分钟量级），期间启动/健康检查较慢属预期；
   - 之后的模块命中 `runtime/.uv-cache`，硬链接装配，显著加快。

**不做清理会怎样**：哈希失配仍会触发自动重装（行为正确），但旧 venv 残留文件与全局缓存继续占盘，且旧版本包残留可能与新版本共存于同一 venv。因此推荐上面的干净迁移。

## 3. 磁盘收益预期（§3.2 实测基线）

统一前（本机实测）：模块 venvs 合计约 6.8G（deep-filter 单模块 torch 即 4.8G）+ 全局 uv 缓存约 6.7G，**双份合计 ~13.5G**。

统一后：

- torch 全家桶物理上只有 **~4G 一份**（在 `runtime/.uv-cache`），deep-filter 与 qwen3-tts（~4G 级）共享同一份物理拷贝；
- 每个模块 venv 只剩薄壳 ~1G（解释器、不共享的小包、硬链接条目）；
- 缓存与 venvs 不再双份，`du` 口径下降 ~6G。

> 计量提示：Linux `du` 默认对硬链接只计一次；Windows 资源管理器"属性"按目录项各计一次（看磁盘实际占用以 `du` 类工具或卷空闲空间变化为准）。

## 4. constraints 自定义与停用

`config/constraints.txt` 是用户可编辑面：

- **改版本**：直接编辑版本行（如未来升级 torch 全家桶时，三行 torch/torchvision/torchaudio **一起**改成新的同族版本）。保存后各模块哈希失配 → 下次启动自动按新约束重装，无需其他操作。
- **增删条目**：可按需为其他共享依赖追加约束行；注意 constraints 只约束"会被安装的包"，未被任何模块依赖的行无作用也无副作用。
- **停用**：两种等价方式——
  - `config/app.toml` 设 `[python] constraints = ""`（显式空字符串）；
  - 或删除/重命名 `config/constraints.txt`（文件不存在时安装与哈希均静默跳过）。
  停用后各模块按各自 requirements.txt 自由解析，跨模块版本可能分叉 → 硬链接去重收益随之消失。
- **索引说明**：constraints 文件不能携带 `--index-url`/`--extra-index-url`（pip/uv 的 -c 文件只接受依赖行）。需要 PyTorch cu130 索引（`https://download.pytorch.org/whl/cu130`）等特定 wheel 源时，由模块 requirements.txt 自带 `--extra-index-url` 行或 uv 配置（`UV_EXTRA_INDEX_URL`/uv.toml/pip.conf）承担。

## 5. 故障排查

### 5.1 硬链接未生效（回退 copy）的识别

`--link-mode hardlink` 在跨文件系统/不支持硬链接的场景由 uv **内建自动回退 copy**（不会安装失败），但去重收益消失。识别方法：

- **日志**：uv 安装输出含 hardlink 失败回退 copy 的警告字样；
- **磁盘**：第二个 torch 模块装完后占用未见显著下降（仍新增 ~4G 量级）；
- **直接验证**：
  ```bash
  # Linux：venv 内文件链接数 >1 即硬链接生效
  stat -c '%h %n' runtime/venvs/<module>/lib/python*/site-packages/torch/version.py
  ```
  ```powershell
  # Windows：列出文件的硬链接条目，多于 1 条即生效
  fsutil hardlink list runtime\venvs\<module>\Lib\site-packages\torch\version.py
  ```

常见诱因与处置：

| 诱因 | 处置 |
|---|---|
| `[python].uv_cache_dir` 被改到与 `runtime/` 不同的磁盘/卷 | 恢复默认 `runtime/.uv-cache`（与 venv 同盘是硬链接前提） |
| 文件系统不支持硬链接（exFAT/FAT32、部分网络挂载、某些容器 overlay 层） | 将应用目录迁到 NTFS/ext4 等本地文件系统；或接受 copy 模式（功能不受影响） |

### 5.2 其他常见现象

- **constraints 改动后每个模块都重装一次**：预期行为（哈希联动），非故障；重装完成后恢复秒判就绪。
- **首次安装中途磁盘不足**：安装过程中缓存 + venv 并存需要约 8G 临时余量；完成后可按 §2 清理任何遗留旧目录。
- **某模块依赖与 constraints 冲突（解析失败）**：临时停用 constraints（§4）恢复安装，随后评估是把 constraints 升到兼容版本还是调整该模块 requirements；欢迎将冲突组合反馈至项目维护。
