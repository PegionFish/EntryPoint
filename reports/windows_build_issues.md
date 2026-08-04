# Windows 编译打包问题记录（GUI 包）

> 记录时间: 2026-08-04
> 环境: Windows, rustc/cargo 1.97.1, Git 分支 master (d011e17)
> 背景: 在 Windows 下执行 `build.ps1 gui`（Clippy → 测试 → release 编译 → 打包）时发现的阻塞问题。
>
> **状态更新（2026-08-04 晚）**: 整合包执行计划（docs/PACK_UNIFY_PLAN.md §15.2）要求 Windows 门禁全绿，两个问题均已修复：
> - 问题 1：`test_cleanup_hf_cache_skips_when_top_symlink` 已加 `#[cfg(unix)]`（`cargo clippy --workspace --all-targets` 已在 Windows 通过）
> - 问题 2：build.ps1 三处错误分支已改为"先全量输出命中行再统一退出"，clippy 分支匹配改为行首锚定 `^(warning:|error(\[|:))`

---

## 问题 1（阻塞）：ep-core 测试目标在 Windows 下编译失败

- **现象**: `cargo clippy --workspace --all-targets` 与 `cargo test --workspace` 在 Windows 下编译失败：

  ```text
  error[E0433]: cannot find `unix` in `os`
      --> crates\ep-core\src\model.rs:2518:18
       |
  2518 |         std::os::unix::fs::symlink(
       |                  ^^^^ could not find `unix` in `os`
  ...
  error: could not compile `ep-core` (lib test) due to 1 previous error
  ```

- **位置**: `crates/ep-core/src/model.rs`，测试 `test_cleanup_hf_cache_skips_when_top_symlink`（约 2502-2531 行）。
- **原因**: 该测试调用 `std::os::unix::fs::symlink`（Windows 标准库不存在此模块）。测试内虽有运行时守卫 `if cfg!(windows) { return; }`，但无法阻止**编译期**解析失败——运行时早退对编译错误无效。
- **影响**: 仅测试目标（lib test）无法编译；库本体与二进制不受影响，`cargo build -p ep-desktop --release` 不受阻塞。但 `build.ps1` 默认流程中的 Clippy（`--all-targets`）与测试两步在 Windows 下必然失败。
- **修复建议**: 给该测试加 `#[cfg(unix)]` 编译期属性（仓库已有先例：`crates/ep-daemon/src/api/models.rs:1168` 的 `#[cfg(unix)]`），并移除已成死代码的 `if cfg!(windows) { return; }` 运行时守卫。
- **排查备注**: 全仓库 `std::os::unix` 仅 2 处引用，另一处（`ep-daemon/src/api/models.rs:1170`）已正确使用 `#[cfg(unix)]` 守卫；model.rs 中其余 5 处 `if cfg!(windows)` 守卫的测试未引用 Unix-only API，不受影响。
- **对照验证**: `cargo clippy --workspace`（不含 `--all-targets`，仅 lib+bin）在 Windows 下零警告通过——确认问题仅存在于测试目标。

## 问题 2：build.ps1 错误过滤器误匹配，提前退出并吞掉真实错误

- **现象**: Clippy 实际编译失败时，脚本仅输出 `[FAIL] Compiling thiserror v2.0.19` 即退出，真实错误（上述 E0433）未显示。
- **位置**: `build.ps1` Clippy 失败分支：
  ```powershell
  $clippyOutput | Where-Object { $_ -match "warning:|error" } | ForEach-Object { Write-Err "  $_" }
  ```
- **原因**: `-match "warning:|error"` 是子串匹配，任何包含 `error` 子串的行都会命中——例如 `Compiling thiserror v2.0.19`（"this**error**"）。且 `Write-Err` 内含 `exit 1`，第一条命中行（误报）就终止脚本，后续真实错误行全部丢失。测试失败分支的 `-match "FAILED|failures:|error\["` 同样存在子串误匹配风险（较低）。
- **影响**: Windows 下排障时被误导（表面看是 thiserror crate 问题，实际是 ep-core 测试代码问题）。
- **修复建议**: 改为行首锚定匹配，例如 `-match '^(warning:|error(\[|:))'`；并将 `Write-Err` 的 `exit 1` 从循环中移出，先输出全部命中行再统一退出。

---

## 规避方式（不改代码继续打包）

使用脚本自带的跳过开关绕过两个失败步骤：

```powershell
.\build.ps1 gui -SkipTest -SkipClippy
```

该路径只执行 release 编译 + 打包，不受上述问题影响。

---

## 环境限制 3（记录于整合包执行期间）：Linux target 交叉 check 被 openssl-sys 阻断

- **现象**: 在 Windows 主机上 `cargo check --workspace --all-targets --target x86_64-unknown-linux-gnu` 失败于 `openssl-sys` build script。
- **原因**: Linux target 下 reqwest 默认 native-tls → openssl-sys 需要系统 OpenSSL 头文件/库与交叉 C 工具链，Windows 主机两者皆无。
- **结论**: workspace 级 Linux 交叉编译验证在本机不可行；双平台保障改为：①代理开发提示词内置 cfg 分支纪律（Unix-only/Windows-only API 必须 #[cfg] 守卫）；②新增依赖纪律（优先纯 Rust，避免引入新的 native 系统依赖）；③纯 Rust crate（如 ep-pack）可单独交叉 check；④最终 Linux 侧验证留待 Linux 环境。
- **备选方案（未采用）**: reqwest 换 rustls 可解除 openssl 依赖，但涉及证书库语义变化（webpki-roots vs 系统 CA，影响企业镜像源/代理场景），属设计决策，本次不做。
