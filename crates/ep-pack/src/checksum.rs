//! `CHECKSUMS.toml` 校验和表 — 生成 / 序列化 / 全量校验（§4.2、§4.4）。
//!
//! 实现所有者：Wave 1 **A4 (PackIO)**。
//!
//! 契约要点：
//! - 条目为「归档内相对路径（恒正斜杠分隔）→ sha256 小写 hex」；
//! - `CHECKSUMS.toml` 自身不入表（无法自哈希），校验时也跳过根级该文件；
//! - 全量校验区分 **缺失 / 多余 / 篡改** 三类问题，全部收集进 [`VerifyReport`]
//!   一次性返回（导入侧可整体呈现适配/错误报告，而非逐条失败）；
//! - 哈希为流式分块读取（1 MiB 缓冲），数 GB 模型权重不整块进内存。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

/// 归档内校验和表的固定文件名（§4.2 归档布局）。
pub const CHECKSUMS_FILE_NAME: &str = "CHECKSUMS.toml";

/// 流式哈希缓冲：1 MiB。
const HASH_CHUNK_SIZE: usize = 1024 * 1024;

/// 计算单个文件的 sha256（流式分块读），返回小写 hex。
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK_SIZE];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// 把 `root` 之下的路径转为归档内相对路径（恒正斜杠分隔）。
///
/// 逐 `components()` 组装，天然剥离平台分隔符（Windows 下不会带反斜杠）；
/// 非 UTF-8 组件无法表示为归档条目名，报错。
fn forward_slash_rel(root: &Path, path: &Path) -> Result<String, ChecksumError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| ChecksumError::PathNotUnderRoot {
            path: path.to_path_buf(),
        })?;
    let mut segs: Vec<&str> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(os) => {
                let seg = os.to_str().ok_or_else(|| ChecksumError::NonUtf8Path {
                    path: path.to_path_buf(),
                })?;
                segs.push(seg);
            }
            // strip_prefix 之后理论上不会出现 ./.. 等组件，防御性拒绝
            _ => {
                return Err(ChecksumError::UnsafePathComponent {
                    path: path.to_path_buf(),
                })
            }
        }
    }
    if segs.is_empty() {
        return Err(ChecksumError::PathNotUnderRoot {
            path: path.to_path_buf(),
        });
    }
    Ok(segs.join("/"))
}

/// 包内容目录遍历结果：全部文件的相对路径（正斜杠、有序）与目录集。
///
/// checksum 生成与 build 打包共用，保证只遍历一次、两处安全语义一致。
#[derive(Debug, Default)]
pub(crate) struct PackTree {
    /// 相对路径（正斜杠）→ 磁盘绝对路径。`BTreeMap` 保证确定性排序。
    pub files: BTreeMap<String, PathBuf>,
    /// 全部目录的相对路径（含空目录；文件祖先目录必然在遍历中出现）。
    pub dirs: BTreeSet<String>,
}

/// 遍历包内容目录并做源侧安全检查。
///
/// 拒绝符号链接（归档不承载 symlink，解包侧亦统一拒绝）与非普通文件
/// （FIFO / 设备等）；跳过根级 `CHECKSUMS.toml`（重建时不哈希旧表）。
pub(crate) fn walk_pack_tree(root: &Path) -> Result<PackTree, ChecksumError> {
    let mut tree = PackTree::default();
    for entry in WalkDir::new(root).follow_links(false).min_depth(1) {
        let entry = entry.map_err(|source| ChecksumError::Walk {
            root: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if entry.path_is_symlink() {
            return Err(ChecksumError::SymlinkInSource {
                path: path.to_path_buf(),
            });
        }
        let rel = forward_slash_rel(root, path)?;
        if entry.file_type().is_dir() {
            tree.dirs.insert(rel);
        } else if entry.file_type().is_file() {
            if rel == CHECKSUMS_FILE_NAME {
                continue; // 校验和表自身不入表（build 时重新生成）
            }
            tree.files.insert(rel, path.to_path_buf());
        } else {
            return Err(ChecksumError::NonRegularFile {
                path: path.to_path_buf(),
            });
        }
    }
    Ok(tree)
}

/// 包内校验和表：文件相对路径（正斜杠）→ sha256 小写 hex。
///
/// TOML 形状（`CHECKSUMS.toml`）：
/// ```toml
/// [checksums]
/// "ep-pack.toml" = "ab12…"
/// "models/m1/weights.bin" = "cd34…"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksumTable {
    #[serde(rename = "checksums")]
    entries: BTreeMap<String, String>,
}

/// 单条篡改记录：期望哈希（表内）与实际哈希（磁盘）不符。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumMismatch {
    pub path: String,
    pub expected: String,
    pub actual: String,
}

/// 全量校验报告：三类问题分别收集（空报告 = 校验通过）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// 表内有条目、磁盘上缺失的文件（正斜杠相对路径）
    pub missing: Vec<String>,
    /// 磁盘上有、表内没有的文件（不含根级 CHECKSUMS.toml）
    pub unexpected: Vec<String>,
    /// 双方都有但 sha256 不符（被篡改）的文件
    pub mismatched: Vec<ChecksumMismatch>,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty() && self.unexpected.is_empty() && self.mismatched.is_empty()
    }
}

impl fmt::Display for VerifyReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} missing, {} unexpected, {} mismatched",
            self.missing.len(),
            self.unexpected.len(),
            self.mismatched.len()
        )?;
        if let Some(p) = self.missing.first() {
            write!(f, "; first missing: `{p}`")?;
        }
        if let Some(p) = self.unexpected.first() {
            write!(f, "; first unexpected: `{p}`")?;
        }
        if let Some(m) = self.mismatched.first() {
            write!(f, "; first mismatched: `{}`", m.path)?;
        }
        Ok(())
    }
}

/// 校验和表错误（生成 / 解析 / 校验）。
#[derive(Debug, thiserror::Error)]
pub enum ChecksumError {
    #[error("failed to walk pack source dir {}: {source}", root.display())]
    Walk {
        root: PathBuf,
        #[source]
        source: walkdir::Error,
    },
    #[error("checksum io error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("pack source contains a symlink (archives must not carry symlinks): {}", path.display())]
    SymlinkInSource { path: PathBuf },
    #[error("pack source contains a non-regular file: {}", path.display())]
    NonRegularFile { path: PathBuf },
    #[error("path escapes the pack root: {}", path.display())]
    PathNotUnderRoot { path: PathBuf },
    #[error("path is not valid UTF-8 (cannot be an archive entry name): {}", path.display())]
    NonUtf8Path { path: PathBuf },
    #[error("path contains an unsafe component: {}", path.display())]
    UnsafePathComponent { path: PathBuf },
    #[error("CHECKSUMS.toml not found at {}", path.display())]
    ChecksumsFileMissing { path: PathBuf },
    #[error("failed to parse CHECKSUMS.toml: {source}")]
    Parse {
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to serialize CHECKSUMS.toml: {source}")]
    Serialize {
        #[source]
        source: toml::ser::Error,
    },
    /// 全量校验失败：缺失 / 多余 / 篡改三类问题见报告字段
    #[error("checksum verification failed: {0}")]
    Integrity(VerifyReport),
}

impl ChecksumTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 条目数（不含 CHECKSUMS.toml 自身）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 查询某相对路径（正斜杠）的期望 sha256。
    pub fn get(&self, rel_path: &str) -> Option<&str> {
        self.entries.get(rel_path).map(String::as_str)
    }

    /// 按确定性排序迭代（相对路径, sha256 hex）。
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// 供 build 使用的构造器：条目表已由打包流程逐文件哈希得到。
    pub(crate) fn from_entries(entries: BTreeMap<String, String>) -> Self {
        Self { entries }
    }

    /// 遍历目录生成校验和表（§4.4「CHECKSUMS 先行」的打包侧）。
    ///
    /// 拒绝符号链接与非普通文件；跳过根级 `CHECKSUMS.toml`。
    pub fn generate(root: &Path) -> Result<Self, ChecksumError> {
        let tree = walk_pack_tree(root)?;
        let mut entries = BTreeMap::new();
        for (rel, path) in tree.files {
            let digest = sha256_file(&path).map_err(|source| ChecksumError::Io {
                path: path.clone(),
                source,
            })?;
            entries.insert(rel, digest);
        }
        Ok(Self { entries })
    }

    /// 读取目录下的 `CHECKSUMS.toml`（导入侧：解包后先验后落位）。
    pub fn read(root: &Path) -> Result<Self, ChecksumError> {
        let path = root.join(CHECKSUMS_FILE_NAME);
        let text = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => ChecksumError::ChecksumsFileMissing { path: path.clone() },
            _ => ChecksumError::Io {
                path: path.clone(),
                source: e,
            },
        })?;
        Self::from_toml_str(&text)
    }

    /// 解析 CHECKSUMS.toml 文本。
    pub fn from_toml_str(text: &str) -> Result<Self, ChecksumError> {
        toml::from_str(text).map_err(|source| ChecksumError::Parse { source })
    }

    /// 序列化为 CHECKSUMS.toml 文本（条目按键排序，输出确定）。
    pub fn to_toml_string(&self) -> Result<String, ChecksumError> {
        toml::to_string(self).map_err(|source| ChecksumError::Serialize { source })
    }

    /// 全量校验：对 `root` 逐文件重算 sha256 并与表比对。
    ///
    /// 三类问题（缺失 / 多余 / 篡改）全部收集后以
    /// [`ChecksumError::Integrity`] 一次性返回；全部通过则 `Ok(())`。
    pub fn verify(&self, root: &Path) -> Result<(), ChecksumError> {
        let tree = walk_pack_tree(root)?;
        let mut report = VerifyReport::default();

        for (rel, path) in &tree.files {
            let actual = sha256_file(path).map_err(|source| ChecksumError::Io {
                path: path.clone(),
                source,
            })?;
            match self.entries.get(rel) {
                None => report.unexpected.push(rel.clone()),
                Some(expected) if expected != &actual => {
                    report.mismatched.push(ChecksumMismatch {
                        path: rel.clone(),
                        expected: expected.clone(),
                        actual,
                    });
                }
                Some(_) => {}
            }
        }
        for rel in self.entries.keys() {
            if !tree.files.contains_key(rel) {
                report.missing.push(rel.clone());
            }
        }

        if report.is_ok() {
            Ok(())
        } else {
            Err(ChecksumError::Integrity(report))
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 各测试独立目录：std::env::temp_dir + join（Windows 反斜杠安全）
    fn unique_root(tag: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-pack-checksum-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_file(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn sha256_known_vector() {
        let root = unique_root("known-vector");
        let f = root.join("abc.txt");
        write_file(&f, b"abc");
        // SHA-256("abc") 标准向量
        assert_eq!(
            sha256_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sha256_large_file_matches_oneshot() {
        let root = unique_root("large");
        let f = root.join("big.bin");
        // 3 MiB 确定性伪随机内容（> 1 MiB 缓冲，覆盖多块流式路径）
        let data: Vec<u8> = (0..3 * 1024 * 1024u32)
            .map(|i| (((i as u64).wrapping_mul(2654435761)) >> 13) as u8)
            .collect();
        write_file(&f, &data);
        let expect = hex::encode(Sha256::digest(&data));
        assert_eq!(sha256_file(&f).unwrap(), expect);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn generate_and_verify_roundtrip() {
        let root = unique_root("roundtrip");
        write_file(&root.join("ep-pack.toml"), b"[pack]\nid = \"t.p\"\n");
        write_file(&root.join("models").join("m1").join("weights.bin"), &[1, 2, 3]);
        write_file(&root.join("pipelines").join("p s.toml"), b"[pipeline]\n"); // 带空格文件名
        write_file(&root.join("unicode-模型.toml"), b"x");

        let table = ChecksumTable::generate(&root).unwrap();
        assert_eq!(table.len(), 4);
        // 条目名恒正斜杠，绝不带平台分隔符
        for (rel, digest) in table.entries() {
            assert!(!rel.contains('\\'), "entry `{rel}` has backslash");
            assert_eq!(digest.len(), 64);
        }
        assert!(table.get("models/m1/weights.bin").is_some());
        assert!(table.get("pipelines/p s.toml").is_some());
        assert!(table.get("unicode-模型.toml").is_some());
        // BTreeMap 有序 → 条目按键排序
        let keys: Vec<&str> = table.entries().map(|(k, _)| k).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);

        table.verify(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn generate_excludes_preexisting_checksums_file() {
        let root = unique_root("skip-checksums");
        write_file(&root.join("ep-pack.toml"), b"[pack]");
        write_file(&root.join(CHECKSUMS_FILE_NAME), b"[checksums]\n");
        // 嵌套同名文件不受豁免，正常入表
        write_file(&root.join("models").join(CHECKSUMS_FILE_NAME), b"nested");

        let table = ChecksumTable::generate(&root).unwrap();
        assert!(table.get(CHECKSUMS_FILE_NAME).is_none());
        assert!(table.get(&format!("models/{CHECKSUMS_FILE_NAME}")).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn generate_rejects_symlink_in_source() {
        let root = unique_root("symlink-source");
        write_file(&root.join("real.txt"), b"data");
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();
        let err = ChecksumTable::generate(&root).unwrap_err();
        assert!(matches!(err, ChecksumError::SymlinkInSource { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn toml_roundtrip() {
        let mut entries = BTreeMap::new();
        entries.insert("ep-pack.toml".to_string(), "ab".repeat(32));
        entries.insert("models/m1/weights.bin".to_string(), "cd".repeat(32));
        entries.insert("dir with space/文件.toml".to_string(), "ef".repeat(32));
        let table = ChecksumTable::from_entries(entries);

        let text = table.to_toml_string().unwrap();
        assert!(text.contains("[checksums]"));
        let parsed = ChecksumTable::from_toml_str(&text).unwrap();
        assert_eq!(parsed, table);

        // 缺 [checksums] 段 → 解析错误
        assert!(matches!(
            ChecksumTable::from_toml_str("[other]\nx = 1"),
            Err(ChecksumError::Parse { .. })
        ));
    }

    fn tamper_scenario() -> (PathBuf, ChecksumTable) {
        let root = unique_root("verify");
        write_file(&root.join("ep-pack.toml"), b"[pack]\nversion = \"1.0.0\"");
        write_file(&root.join("models").join("m1").join("w.bin"), b"weights-v1");
        write_file(&root.join("pipelines").join("p.toml"), b"[pipeline]");
        let table = ChecksumTable::generate(&root).unwrap();
        (root, table)
    }

    #[test]
    fn verify_detects_tampered_file() {
        let (root, table) = tamper_scenario();
        write_file(&root.join("models").join("m1").join("w.bin"), b"weights-EVIL");
        let err = table.verify(&root).unwrap_err();
        match err {
            ChecksumError::Integrity(report) => {
                assert_eq!(report.missing.len(), 0);
                assert_eq!(report.unexpected.len(), 0);
                assert_eq!(report.mismatched.len(), 1);
                let m = &report.mismatched[0];
                assert_eq!(m.path, "models/m1/w.bin");
                assert_eq!(m.expected, *table.get("models/m1/w.bin").unwrap());
                assert_ne!(m.expected, m.actual);
            }
            other => panic!("expected Integrity, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_detects_missing_file() {
        let (root, table) = tamper_scenario();
        std::fs::remove_file(root.join("pipelines").join("p.toml")).unwrap();
        let err = table.verify(&root).unwrap_err();
        match err {
            ChecksumError::Integrity(report) => {
                assert_eq!(report.missing, vec!["pipelines/p.toml".to_string()]);
                assert!(report.unexpected.is_empty());
                assert!(report.mismatched.is_empty());
            }
            other => panic!("expected Integrity, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_detects_unexpected_file() {
        let (root, table) = tamper_scenario();
        write_file(&root.join("models").join("m1").join("extra.bin"), b"surprise");
        let err = table.verify(&root).unwrap_err();
        match err {
            ChecksumError::Integrity(report) => {
                assert!(report.missing.is_empty());
                assert_eq!(report.unexpected, vec!["models/m1/extra.bin".to_string()]);
                assert!(report.mismatched.is_empty());
            }
            other => panic!("expected Integrity, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_reports_all_three_classes_at_once() {
        let (root, table) = tamper_scenario();
        // 篡改 + 缺失 + 多余同时存在 → 一次报全
        write_file(&root.join("models").join("m1").join("w.bin"), b"tampered");
        std::fs::remove_file(root.join("pipelines").join("p.toml")).unwrap();
        write_file(&root.join("dropped.txt"), b"extra");

        match table.verify(&root).unwrap_err() {
            ChecksumError::Integrity(report) => {
                assert_eq!(report.missing, vec!["pipelines/p.toml".to_string()]);
                assert_eq!(report.unexpected, vec!["dropped.txt".to_string()]);
                assert_eq!(report.mismatched.len(), 1);
                assert!(!report.is_ok());
                // Display 汇总三类计数
                let msg = report.to_string();
                assert!(msg.contains("1 missing"));
                assert!(msg.contains("1 unexpected"));
                assert!(msg.contains("1 mismatched"));
            }
            other => panic!("expected Integrity, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_missing_checksums_file_is_specific_error() {
        let root = unique_root("read-missing");
        let err = ChecksumTable::read(&root).unwrap_err();
        assert!(
            matches!(err, ChecksumError::ChecksumsFileMissing { .. }),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
