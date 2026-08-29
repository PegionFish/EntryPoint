//! ep-pack CLI 集成测试 — 通过真实二进制（`CARGO_BIN_EXE_ep-pack`）驱动，
//! tempdir 全链覆盖（任务 C6 第 8 项）：
//!
//! - new → validate → build → info → import → validate 全链
//! - bundle + reference 混合包导入（meta/注册表/适配报告/管线落位）
//! - dry-run 只出报告不落位
//! - 恶意包拒绝（zip-slip / checksum 篡改 / 缺 CHECKSUMS）
//! - 缺模块 → 适配报告 Unsupported（§4.4 报而不炸）
//! - export 已装包重建 .zip 往返
//! - 用法错误退出码 2 / 校验失败退出码 1
//!
//! 跨平台纪律：路径一律 Path::join；退出码断言双平台一致。
//! 测试统一设 `EP_PACK_CLI_NO_DEVICE_DETECT=1`：适配报告不依赖本机设备，
//! 判定确定（无设备视角 → CPU 保底/不支持）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use zip::write::SimpleFileOptions;
use zip::ZipWriter;

static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

// ─── 基建 ────────────────────────────────────────────────────────────────────

fn temp_root(tag: &str) -> PathBuf {
    let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "ep-pack-cli-it-{tag}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

fn ep_pack() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ep-pack"));
    cmd.env("EP_PACK_CLI_NO_DEVICE_DETECT", "1");
    cmd
}

struct CmdOut {
    code: i32,
    stdout: String,
    stderr: String,
}

impl CmdOut {
    fn assert_code(&self, expected: i32, ctx: &str) {
        assert_eq!(
            self.code, expected,
            "{ctx}: expected exit {expected}, got {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.code, self.stdout, self.stderr
        );
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout is not valid JSON: {e}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                self.stdout, self.stderr
            )
        })
    }
}

fn run(cmd: &mut Command) -> CmdOut {
    let out = cmd.output().expect("failed to run ep-pack binary");
    CmdOut {
        code: out.status.code().expect("ep-pack terminated without exit code"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn write_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn read_json_file(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!("{} is not valid JSON: {e}", path.display())
    })
}

// ─── fixtures ────────────────────────────────────────────────────────────────

/// 模块 manifest fixture（qualified_id 按 §4.3；target_dir 与权重目录对应）。
fn module_toml(
    module_id: &str,
    backends: &str,
    qualified_id: &str,
    variant: &str,
    target_dir: &str,
    repo_id: &str,
) -> String {
    format!(
        r#"
[module]
id = "{module_id}"
name = "{module_id}"
version = "1.0.0"
description = "cli integration test module"
category = "asr"
genre = "test"

[runtime]
type = "python"

[compute]
backends = [{backends}]

[[models]]
id = "{variant}"
name = "{variant}"
source = "huggingface"
repo_id = "{repo_id}"
target_dir = "{target_dir}"
qualified_id = "{qualified_id}"

[interface]
type = "http"
"#
    )
}

/// 写双模块目录：asr（cuda+cpu）+ tts（cpu）
fn write_module_fixtures(mods: &Path) {
    write_file(
        &mods.join("asr").join("module.toml"),
        module_toml("asr", "\"cuda\", \"cpu\"", "ep.acme.asr", "v1", "asr-v1", "acme/asr-v1")
            .as_bytes(),
    );
    write_file(
        &mods.join("tts").join("module.toml"),
        module_toml("tts", "\"cpu\"", "ep.acme.tts", "v2", "tts-v2", "acme/tts-v2").as_bytes(),
    );
}

const DEMO_MANIFEST: &str = r#"
[pack]
id = "tester.demo-pack"
version = "1.0.0"
name = "Demo Pack"
description = "cli integration test pack"
authors = ["c6"]
min_ep_version = "0.1.0"
tags = ["demo"]

[compute]
backends = ["cuda", "cpu"]

[[models]]
qualified_id = "ep.acme.asr"
variant = "v1"
mode = "bundle"
tags = ["asr"]

[[models]]
qualified_id = "ep.acme.tts"
variant = "v2"
mode = "reference"

[[pipelines]]
file = "pipelines/main.toml"
"#;

/// §6.2 完整形状管线（file_input → module → file_output，通过 DAG 校验）
const DEMO_PIPELINE: &str = r#"
[pipeline]
id = "demo-main"
name = "Demo Main"
description = "integration test pipeline"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "asr"
kind = "module"
module_id = "asr"
capability = "transcribe"
model = "ep.acme.asr@v1"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "out"]
to = ["asr", "in"]

[[edges]]
from = ["asr", "out"]
to = ["output", "in"]
"#;

/// 写标准包源目录（bundle 权重 + 管线），返回源目录路径。
fn write_demo_source(root: &Path, with_weights: bool) -> PathBuf {
    let src = root.join("src");
    write_file(&src.join("ep-pack.toml"), DEMO_MANIFEST.as_bytes());
    write_file(&src.join("pipelines").join("main.toml"), DEMO_PIPELINE.as_bytes());
    if with_weights {
        write_file(
            &src.join("models").join("asr-v1").join("weights.bin"),
            b"weights-v1",
        );
    }
    src
}

/// 构建 demo 包归档（CLI build），返回 .zip 路径。
fn build_demo_archive(root: &Path) -> PathBuf {
    let src = write_demo_source(root, true);
    let archive = root.join("demo.zip");
    let out = run(ep_pack()
        .current_dir(root)
        .args(["build"])
        .arg(&src)
        .args(["-o"])
        .arg(&archive)
        .arg("--json"));
    out.assert_code(0, "build demo pack");
    archive
}

/// 直接写（可恶意的）zip 条目
fn write_raw_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    for (name, content) in entries {
        zip.start_file(*name, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content).unwrap();
    }
    zip.finish().unwrap();
}

// ─── 1. new → validate → build → info → import → validate 全链 ─────────────

#[test]
fn scaffold_full_chain() {
    let root = temp_root("scaffold-chain");

    // new：脚手架落盘
    let pack_dir = root.join("mypack");
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["new", "mypack", "--json"]));
    out.assert_code(0, "new");
    assert!(pack_dir.join("ep-pack.toml").is_file());
    assert!(pack_dir.join("README.md").is_file());
    assert!(pack_dir.join("pipelines").join("example.toml").is_file());
    assert!(pack_dir.join("models").is_dir());

    // new：非空目录拒绝（用法错误 2）
    let out = run(ep_pack().current_dir(&root).args(["new", "mypack"]));
    out.assert_code(2, "new on non-empty dir");

    // validate：模板即通过（含 §6.2 样例管线）
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["validate", "mypack", "--json"]));
    out.assert_code(0, "validate scaffold");
    assert_eq!(out.json()["ok"], true);

    // build：缺省输出名 <id>-<version>.zip 于当前目录
    let out = run(ep_pack().current_dir(&root).args(["build", "mypack", "--json"]));
    out.assert_code(0, "build scaffold");
    let archive = root.join("your-name.my-pack-0.1.0.zip");
    assert!(archive.is_file(), "default archive name missing");

    // info（归档模式）：清单摘要 + 文件清单 + CHECKSUMS OK
    let out = run(ep_pack().current_dir(&root).args(["info"]).arg(&archive).arg("--json"));
    out.assert_code(0, "info archive");
    let j = out.json();
    assert_eq!(j["kind"], "archive");
    assert_eq!(j["manifest"]["id"], "your-name.my-pack");
    assert_eq!(j["checksums"]["ok"], true);
    let files: Vec<&str> = j["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"pipelines/example.toml"), "{files:?}");
    assert!(files.contains(&"README.md"), "{files:?}");

    // import：纯管线包导入空白根（无模块也能装——无模型条目）
    let target = root.join("ep-root");
    std::fs::create_dir_all(&target).unwrap();
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&archive)
        .args(["--root"])
        .arg(&target)
        .arg("--json"));
    out.assert_code(0, "import scaffold pack");
    let j = out.json();
    assert_eq!(j["dry_run"], false);
    assert_eq!(j["pack_id"], "your-name.my-pack");
    assert_eq!(j["pipelines_installed"], serde_json::json!(["example"]));

    // 落位断言：注册表 + 管线文件
    let registry = target
        .join("runtime")
        .join("packs")
        .join("your-name.my-pack.json");
    assert!(registry.is_file(), "registry entry missing");
    assert!(target.join("config").join("pipelines").join("example.toml").is_file());

    // info（注册表模式）
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["info", "your-name.my-pack", "--root"])
        .arg(&target)
        .arg("--json"));
    out.assert_code(0, "info installed");
    let j = out.json();
    assert_eq!(j["kind"], "installed");
    assert_eq!(j["pack"]["id"], "your-name.my-pack");
    assert_eq!(j["pack"]["pipelines"], serde_json::json!(["example"]));

    // 末尾再 validate（源目录未被污染）
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["validate", "mypack"]));
    out.assert_code(0, "final validate");

    cleanup(&root);
}

// ─── 2. bundle + reference 混合导入全链 ─────────────────────────────────────

#[test]
fn import_bundle_and_reference_models() {
    let root = temp_root("import-mixed");
    let archive = build_demo_archive(&root);
    let mods = root.join("mods");
    write_module_fixtures(&mods);
    let target = root.join("ep-root");
    std::fs::create_dir_all(&target).unwrap();

    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&archive)
        .args(["--root"])
        .arg(&target)
        .args(["--modules-dir"])
        .arg(&mods)
        .arg("--json"));
    out.assert_code(0, "import mixed pack");
    let j = out.json();

    // 适配报告：无设备视角（测试钩子）→ 两条均 CPU 保底
    let adaptation = j["adaptation"].as_array().unwrap();
    assert_eq!(adaptation.len(), 2);
    for a in adaptation {
        assert_eq!(a["verdict"], "cpu_fallback", "{a}");
    }

    // bundle 落位 + reference 待下载
    assert_eq!(j["installed_models"].as_array().unwrap().len(), 1);
    assert_eq!(j["installed_models"][0]["target_dir"], "asr-v1");
    let pending = j["pending_downloads"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["qualified_id"], "ep.acme.tts");
    assert_eq!(pending[0]["source"], "huggingface");
    assert_eq!(pending[0]["location"], "acme/tts-v2");
    assert_eq!(j["pipelines_installed"], serde_json::json!(["demo-main"]));
    assert_eq!(
        j["pipeline_dependencies"],
        serde_json::json!(["ep.acme.asr@v1"])
    );

    // 文件系统断言
    let weights = target.join("models").join("asr-v1").join("weights.bin");
    assert_eq!(std::fs::read(&weights).unwrap(), b"weights-v1");
    let meta = read_json_file(&target.join("models").join("asr-v1").join(".ep_meta.json"));
    assert_eq!(meta["source"], "pack");
    assert_eq!(meta["pack_id"], "tester.demo-pack");
    assert_eq!(meta["qualified_id"], "ep.acme.asr");
    assert_eq!(meta["module_id"], "asr");
    // tags 合并（条目 asr + 包级 demo）
    let tags = meta["tags"].as_array().unwrap();
    assert!(tags.contains(&serde_json::json!("asr")));
    assert!(tags.contains(&serde_json::json!("demo")));

    // reference 模型不落位
    assert!(!target.join("models").join("tts-v2").exists());

    // 注册表
    let registry = read_json_file(
        &target.join("runtime").join("packs").join("tester.demo-pack.json"),
    );
    assert_eq!(registry["models"].as_array().unwrap().len(), 2);
    assert_eq!(registry["pipelines"], serde_json::json!(["demo-main"]));

    // 管线落位
    assert!(target.join("config").join("pipelines").join("main.toml").is_file());

    // 重复导入 → 已安装硬失败（退出码 1）
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&archive)
        .args(["--root"])
        .arg(&target)
        .args(["--modules-dir"])
        .arg(&mods)
        .arg("--json"));
    out.assert_code(1, "re-import same pack");
    let j = out.json();
    assert_eq!(j["ok"], false);
    assert!(
        j["errors"][0].as_str().unwrap().contains("already installed"),
        "{}",
        j["errors"][0]
    );

    cleanup(&root);
}

// ─── 3. dry-run：只出适配/校验报告，绝不落位 ────────────────────────────────

#[test]
fn dry_run_reports_without_placing() {
    let root = temp_root("dry-run");
    let archive = build_demo_archive(&root);
    let mods = root.join("mods");
    write_module_fixtures(&mods);
    let target = root.join("ep-root");
    std::fs::create_dir_all(&target).unwrap();

    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&archive)
        .args(["--root"])
        .arg(&target)
        .args(["--modules-dir"])
        .arg(&mods)
        .args(["--dry-run", "--json"]));
    out.assert_code(0, "dry-run");
    let j = out.json();
    assert_eq!(j["dry_run"], true);
    assert_eq!(j["ok"], true);
    assert_eq!(j["pack_id"], "tester.demo-pack");
    assert_eq!(j["adaptation"].as_array().unwrap().len(), 2);
    assert_eq!(j["pending_downloads"].as_array().unwrap().len(), 1);
    assert_eq!(j["pipelines_installable"], serde_json::json!(["demo-main"]));

    // 绝不落位：模型目录 / 注册表 / 管线目录均不得出现
    assert!(!target.join("models").join("asr-v1").exists());
    assert!(!target
        .join("runtime")
        .join("packs")
        .join("tester.demo-pack.json")
        .exists());
    assert!(!target.join("config").join("pipelines").join("main.toml").exists());

    // dry-run 之后正式导入仍然成功（状态未被污染）
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&archive)
        .args(["--root"])
        .arg(&target)
        .args(["--modules-dir"])
        .arg(&mods)
        .arg("--json"));
    out.assert_code(0, "import after dry-run");
    assert!(target.join("models").join("asr-v1").join("weights.bin").is_file());

    cleanup(&root);
}

// ─── 4. 恶意包拒绝 ──────────────────────────────────────────────────────────

#[test]
fn import_rejects_zip_slip() {
    let root = temp_root("zip-slip");
    let evil = root.join("evil.zip");
    write_raw_zip(
        &evil,
        &[
            ("ep-pack.toml", b"[pack]\nid = \"a.b\"\nversion = \"1.0.0\"\n"),
            ("../evil.txt", b"pwned"),
        ],
    );
    let target = root.join("ep-root");
    std::fs::create_dir_all(&target).unwrap();

    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&evil)
        .args(["--root"])
        .arg(&target)
        .arg("--json"));
    out.assert_code(1, "zip-slip import");
    assert_eq!(out.json()["ok"], false);
    // 逃逸文件绝不得出现
    assert!(!root.join("evil.txt").exists());
    assert!(!target.join("evil.txt").exists());

    cleanup(&root);
}

#[test]
fn import_rejects_checksum_tamper_and_missing() {
    let root = temp_root("checksum");
    let target = root.join("ep-root");
    std::fs::create_dir_all(&target).unwrap();

    // 4a. CHECKSUMS 篡改：哈希与内容不符
    let tampered = root.join("tampered.zip");
    write_raw_zip(
        &tampered,
        &[
            ("ep-pack.toml", DEMO_MANIFEST.as_bytes()),
            ("models/asr-v1/weights.bin", b"evil-weights"),
            (
                "CHECKSUMS.toml",
                b"[checksums]\n\"ep-pack.toml\" = \"0000000000000000000000000000000000000000000000000000000000000000\"\n\"models/asr-v1/weights.bin\" = \"1111111111111111111111111111111111111111111111111111111111111111\"\n",
            ),
        ],
    );
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&tampered)
        .args(["--root"])
        .arg(&target)
        .arg("--json"));
    out.assert_code(1, "tampered checksums");
    let msg = out.json()["errors"][0].as_str().unwrap().to_string();
    assert!(msg.contains("checksum"), "{msg}");
    // 校验先于落位：模型目录不得出现
    assert!(!target.join("models").join("asr-v1").exists());

    // 4b. 缺 CHECKSUMS.toml：导入硬失败
    let no_checksums = root.join("no-checksums.zip");
    write_raw_zip(
        &no_checksums,
        &[("ep-pack.toml", DEMO_MANIFEST.as_bytes())],
    );
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&no_checksums)
        .args(["--root"])
        .arg(&target)
        .arg("--json"));
    out.assert_code(1, "missing checksums");

    cleanup(&root);
}

// ─── 5. 缺模块 → 适配报告 Unsupported（§4.4 报而不炸）─────────────────────

#[test]
fn import_missing_module_reports_unsupported() {
    let root = temp_root("missing-module");
    let archive = build_demo_archive(&root);
    let target = root.join("ep-root");
    std::fs::create_dir_all(&target).unwrap();
    let empty_mods = root.join("empty-mods");
    std::fs::create_dir_all(&empty_mods).unwrap();

    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&archive)
        .args(["--root"])
        .arg(&target)
        .args(["--modules-dir"])
        .arg(&empty_mods)
        .arg("--json"));
    out.assert_code(0, "import with no modules");
    let j = out.json();
    let adaptation = j["adaptation"].as_array().unwrap();
    assert_eq!(adaptation.len(), 2);
    for a in adaptation {
        assert_eq!(a["verdict"], "unsupported", "{a}");
    }
    assert_eq!(j["installed_models"].as_array().unwrap().len(), 0);
    // 管线仍然落位（管线不依赖模块存在性）
    assert_eq!(j["pipelines_installed"], serde_json::json!(["demo-main"]));
    // bundle 未解析 → 未落位
    assert!(!target.join("models").join("asr-v1").exists());
    // 注册表照写（包已安装，仅模型未适配）
    assert!(target
        .join("runtime")
        .join("packs")
        .join("tester.demo-pack.json")
        .is_file());

    cleanup(&root);
}

// ─── 6. validate 错误列表 ───────────────────────────────────────────────────

#[test]
fn validate_collects_errors() {
    let root = temp_root("validate-errors");

    // 6a. 清单多处非法 + 管线文件缺失
    let bad = root.join("bad");
    write_file(
        &bad.join("ep-pack.toml"),
        br#"
[pack]
id = "Bad Id"
version = "1.0"
name = "n"
description = "d"

[compute]
backends = ["cpu"]

[[models]]
qualified_id = "NOPE"
variant = "v1"
mode = "reference"

[[pipelines]]
file = "pipelines/missing.toml"
"#,
    );
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["validate"])
        .arg(&bad)
        .arg("--json"));
    out.assert_code(1, "validate bad manifest");
    let j = out.json();
    let errors: Vec<String> = j["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect();
    assert!(errors.iter().any(|e| e.contains("pack.id")), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("pack.version")), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("qualified_id")), "{errors:?}");
    assert!(
        errors.iter().any(|e| e.contains("missing.toml")),
        "{errors:?}"
    );

    // 6b. 管线 §6.2 DAG 校验：缺 file_input
    let noinput = root.join("noinput");
    write_file(
        &noinput.join("ep-pack.toml"),
        br#"
[pack]
id = "a.b"
version = "1.0.0"
name = "n"
description = "d"

[compute]
backends = ["cpu"]

[[pipelines]]
file = "pipelines/p.toml"
"#,
    );
    write_file(
        &noinput.join("pipelines").join("p.toml"),
        br#"
[pipeline]
id = "p"
name = "P"

[[nodes]]
id = "m"
kind = "module"
module_id = "asr"
capability = "transcribe"
"#,
    );
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["validate"])
        .arg(&noinput)
        .arg("--json"));
    out.assert_code(1, "validate pipeline without file_input");
    let errors = out.json()["errors"].as_array().unwrap().clone();
    assert!(
        errors.iter().any(|e| e.as_str().unwrap().contains("file_input")),
        "{errors:?}"
    );

    // 6c. CHECKSUMS 若存在则校验：列了不存在的文件 → 报错
    let stale = root.join("stale");
    write_file(
        &stale.join("ep-pack.toml"),
        b"[pack]\nid = \"a.b\"\nversion = \"1.0.0\"\nname = \"n\"\ndescription = \"d\"\n\n[compute]\nbackends = [\"cpu\"]\n",
    );
    write_file(
        &stale.join("CHECKSUMS.toml"),
        b"[checksums]\n\"ghost.txt\" = \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
    );
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["validate"])
        .arg(&stale)
        .arg("--json"));
    out.assert_code(1, "validate stale checksums");
    let errors = out.json()["errors"].as_array().unwrap().clone();
    assert!(
        errors.iter().any(|e| e.as_str().unwrap().contains("ghost.txt")),
        "{errors:?}"
    );

    cleanup(&root);
}

// ─── 7. export 往返 ─────────────────────────────────────────────────────────

#[test]
fn export_installed_pack_roundtrip() {
    let root = temp_root("export");
    let archive = build_demo_archive(&root);
    let mods = root.join("mods");
    write_module_fixtures(&mods);
    let target = root.join("ep-root");
    std::fs::create_dir_all(&target).unwrap();

    // 先导装
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&archive)
        .args(["--root"])
        .arg(&target)
        .args(["--modules-dir"])
        .arg(&mods)
        .arg("--json"));
    out.assert_code(0, "import before export");

    // export：注册表 → 重建 .zip（bundle 权重硬链接/复制自 models/）
    let exported = root.join("exported.zip");
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["export", "tester.demo-pack", "--root"])
        .arg(&target)
        .args(["--modules-dir"])
        .arg(&mods)
        .args(["-o"])
        .arg(&exported)
        .arg("--json"));
    out.assert_code(0, "export");
    assert!(exported.is_file());
    let j = out.json();
    assert_eq!(j["pack_id"], "tester.demo-pack");
    assert_eq!(j["models"], 2);

    // info：重建归档 CHECKSUMS 全绿、清单字段还原
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["info"])
        .arg(&exported)
        .arg("--json"));
    out.assert_code(0, "info exported");
    let j = out.json();
    assert_eq!(j["checksums"]["ok"], true);
    assert_eq!(j["manifest"]["id"], "tester.demo-pack");
    assert_eq!(j["manifest"]["version"], "1.0.0");
    let models = j["manifest"]["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert!(models.iter().any(|m| m["mode"] == "bundle"));
    assert!(models.iter().any(|m| m["mode"] == "reference"));
    assert_eq!(j["manifest"]["pipelines"], serde_json::json!(["pipelines/main.toml"]));

    // 往返：导出产物可再次导入新根（bundle 权重随行）
    let target2 = root.join("ep-root-2");
    std::fs::create_dir_all(&target2).unwrap();
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import"])
        .arg(&exported)
        .args(["--root"])
        .arg(&target2)
        .args(["--modules-dir"])
        .arg(&mods)
        .arg("--json"));
    out.assert_code(0, "re-import exported archive");
    assert_eq!(
        std::fs::read(target2.join("models").join("asr-v1").join("weights.bin")).unwrap(),
        b"weights-v1"
    );

    // 未安装 pack id → 用法错误 2
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["export", "ghost.pack", "--root"])
        .arg(&target)
        .arg("--json"));
    out.assert_code(2, "export unknown pack");

    cleanup(&root);
}

// ─── 8. 用法错误与退出码 ────────────────────────────────────────────────────

#[test]
fn usage_errors_exit_code_2() {
    let root = temp_root("usage");

    // 未知子命令
    let out = run(ep_pack().current_dir(&root).args(["frobnicate"]));
    out.assert_code(2, "unknown command");

    // 缺位置参数
    let out = run(ep_pack().current_dir(&root).args(["import"]));
    out.assert_code(2, "import without archive");

    // 不存在的归档
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["import", "no-such.zip"]));
    out.assert_code(2, "import missing archive");

    // info：不存在的文件且注册表无此 id
    let out = run(ep_pack().current_dir(&root).args(["info", "ghost.pack"]));
    out.assert_code(2, "info unknown pack");

    // 未知选项
    let out = run(ep_pack().current_dir(&root).args(["validate", "--frobnicate"]));
    out.assert_code(2, "unknown option");

    // build：无清单目录 → 校验失败（退出码 1，非用法错误）
    let empty = root.join("empty-src");
    std::fs::create_dir_all(&empty).unwrap();
    let out = run(ep_pack()
        .current_dir(&root)
        .args(["build"])
        .arg(&empty));
    out.assert_code(1, "build without manifest");

    // help 退出码 0
    let out = run(ep_pack().current_dir(&root).args(["--help"]));
    out.assert_code(0, "help");
    assert!(out.stdout.contains("ep-pack"));

    cleanup(&root);
}
