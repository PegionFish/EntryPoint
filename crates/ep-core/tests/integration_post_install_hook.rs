//! 集成测试：模块 post-install 钩子（requirements_by_backend 契约缺口，
//! HETERO_DIST_PLAN / MODULE_SPEC §2.6 v1.3-draft）
//!
//! 全程离线：以"假 uv"（记录调用、伪造 venv 解释器布局、恒成功）驱动
//! `ensure_venv*` 完整走通 创建 → 安装 → 钩子 → 写哈希 链路，验证：
//! 脚本缺失静默跳过 / 环境变量注入 / 失败 fail-fast 且哈希不落盘 /
//! 依赖未变不重跑 / 真实用例脚本语法。

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use ep_core::config::PythonConfig;
use ep_core::env::EnvManager;
use ep_core::types::ComputeBackend;

// ─── 夹具 ────────────────────────────────────────────────────────────────────

fn unique_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ep-int-hook-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 伪造 uv：调用追加日志；`venv` 子命令在目标目录伪造 `bin/python`，
/// 其余（pip install 等）直接成功。绝不联网。
fn write_fake_uv(root: &Path) -> PathBuf {
    let log = root.join("fake-uv.log");
    let bin_dir = root.join("fake-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let uv = bin_dir.join("uv");
    fs::write(
        &uv,
        format!(
            "#!/usr/bin/env bash\n\
             echo \"uv $*\" >> '{}'\n\
             if [[ \"${{1:-}}\" == \"venv\" ]]; then\n\
             target=\"${{@: -1}}\"\n\
             mkdir -p \"$target/bin\"\n\
             : > \"$target/bin/python\"\n\
             fi\n\
             exit 0\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
    uv
}

fn mgr_with_fake_uv(root: &Path) -> EnvManager {
    let config = PythonConfig {
        uv_path: write_fake_uv(root).to_string_lossy().into_owned(),
        ..Default::default()
    };
    EnvManager::new(root, &config)
}

/// 模块目录 + requirements + 可选钩子脚本
fn module_with_hook(root: &Path, module_id: &str, hook_body: Option<&str>) -> PathBuf {
    let mod_dir = root.join("modules").join(module_id);
    fs::create_dir_all(mod_dir.join("scripts")).unwrap();
    fs::write(mod_dir.join("requirements.txt"), "fastapi\n").unwrap();
    if let Some(body) = hook_body {
        fs::write(mod_dir.join("scripts").join("post-install.sh"), body).unwrap();
    }
    mod_dir
}

fn hash_file_of(venv_dir: &Path) -> PathBuf {
    venv_dir.join(".ep_deps_hash")
}

// ─── 用例 ────────────────────────────────────────────────────────────────────

/// 无钩子脚本 → 静默跳过，安装主流程照常完成且哈希落盘
#[test]
fn missing_hook_is_silently_skipped() {
    let root = unique_root("skip");
    let mgr = mgr_with_fake_uv(&root);
    let req = module_with_hook(&root, "mod-skip", None).join("requirements.txt");

    let py = mgr
        .ensure_venv_for_backend("mod-skip", "3.12", &req, ComputeBackend::Cuda)
        .expect("无钩子必须静默跳过且整体成功");
    assert!(
        py.to_string_lossy().contains("mod-skip--cuda"),
        "应返回分后端 venv 解释器路径: {}",
        py.display()
    );
    assert!(hash_file_of(&mgr.venv_dir_for_backend("mod-skip", ComputeBackend::Cuda)).exists());

    let _ = fs::remove_dir_all(&root);
}

/// 钩子在依赖安装后执行，收到 VIRTUAL_ENV=<venv 目录> 与 EP_BACKEND=<小写名>；
/// 依赖未变的重入不得重跑钩子（幂等口径）
#[test]
fn hook_receives_injected_env_and_skips_on_reentry() {
    let root = unique_root("env");
    let mgr = mgr_with_fake_uv(&root);
    let env_out = root.join("hook-env.txt");
    let body = format!(
        "#!/usr/bin/env bash\nprintf '%s\\n%s\\n' \"$VIRTUAL_ENV\" \"$EP_BACKEND\" > '{}'\n",
        env_out.display()
    );
    let req = module_with_hook(&root, "mod-env", Some(&body)).join("requirements.txt");

    mgr.ensure_venv_for_backend("mod-env", "3.12", &req, ComputeBackend::Rocm)
        .expect("钩子成功时 ensure_venv 必须成功");

    let venv_dir = mgr.venv_dir_for_backend("mod-env", ComputeBackend::Rocm);
    let content = fs::read_to_string(&env_out).unwrap();
    let mut lines = content.lines();
    assert_eq!(
        lines.next(),
        Some(venv_dir.to_str().unwrap()),
        "VIRTUAL_ENV 必须指向分后端 venv 目录"
    );
    assert_eq!(
        lines.next(),
        Some("rocm"),
        "EP_BACKEND 必须为后端小写名"
    );
    // 钩子成功后才落哈希：哈希存在即代表"安装 + 后处理"完整就绪
    assert!(hash_file_of(&venv_dir).exists());

    // 重入：依赖栈未变 → 不触发安装也不重跑钩子（覆盖标记不被冲掉）
    fs::write(&env_out, "reentry-marker").unwrap();
    mgr.ensure_venv_for_backend("mod-env", "3.12", &req, ComputeBackend::Rocm)
        .expect("重入必须成功");
    assert_eq!(
        fs::read_to_string(&env_out).unwrap(),
        "reentry-marker",
        "依赖未变时钩子不得重跑"
    );

    let _ = fs::remove_dir_all(&root);
}

/// 钩子失败 → 整体报错（fail-fast），且哈希不落盘——半成品依赖栈不得被锁定
#[test]
fn failing_hook_fails_ensure_venv_without_hash() {
    let root = unique_root("fail");
    let mgr = mgr_with_fake_uv(&root);
    let body = "#!/usr/bin/env bash\necho 'overlay boom' >&2\nexit 7\n";
    let req = module_with_hook(&root, "mod-fail", Some(body)).join("requirements.txt");

    let err = mgr
        .ensure_venv_for_backend("mod-fail", "3.12", &req, ComputeBackend::Rocm)
        .expect_err("钩子失败必须整体报错");
    assert!(
        err.to_string().contains("post-install"),
        "错误信息需指明钩子环节: {err}"
    );
    assert!(
        !hash_file_of(&mgr.venv_dir_for_backend("mod-fail", ComputeBackend::Rocm)).exists(),
        "钩子失败时哈希不得落盘（防半成品依赖栈被哈希锁定）"
    );

    let _ = fs::remove_dir_all(&root);
}

/// 旧单 venv 口径（backend=None）：EP_BACKEND 注入空串
#[test]
fn legacy_layout_hook_receives_empty_ep_backend() {
    let root = unique_root("legacy");
    let mgr = mgr_with_fake_uv(&root);
    let env_out = root.join("hook-env-legacy.txt");
    let body = format!(
        "#!/usr/bin/env bash\nprintf '[%s]' \"${{EP_BACKEND:-}}\" > '{}'\n",
        env_out.display()
    );
    let req = module_with_hook(&root, "mod-legacy", Some(&body)).join("requirements.txt");

    mgr.ensure_venv("mod-legacy", "3.12", &req)
        .expect("旧口径钩子成功");
    assert_eq!(
        fs::read_to_string(&env_out).unwrap(),
        "[]",
        "旧口径 None 时 EP_BACKEND 必须为空串"
    );
    let content = fs::read_to_string(root.join("fake-uv.log")).unwrap();
    assert!(
        content.contains("pip install"),
        "夹具自检：必须真实走过安装分支\n{content}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// 真实用例：faster-whisper 的 ROCm 覆盖钩子必须通过 bash -n 语法检查
#[test]
fn faster_whisper_rocm_hook_passes_bash_syntax_check() {
    let hook = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../modules/faster-whisper/scripts/post-install.sh");
    assert!(hook.is_file(), "真实用例钩子必须存在: {}", hook.display());

    let out = Command::new("bash")
        .arg("-n")
        .arg(&hook)
        .output()
        .expect("spawn bash -n");
    assert!(
        out.status.success(),
        "bash -n 语法检查失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
