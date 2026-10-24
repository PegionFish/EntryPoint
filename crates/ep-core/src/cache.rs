//! 直跑结果内容寻址缓存 — 纯函数与目录布局（T-16，无人值守省 GPU/云费）
//!
//! 仅供 `/execute/single` 等**直跑（direct/）**任务使用；管线整体结果不缓存
//! （handoff §5 明确范围）。缓存键在内容哈希可用时为强键：
//!
//! ```text
//! sha256("ep-direct-cache\0v1\0" + module_id + "\0" + capability + "\0"
//!        + canonical_params_json + "\0" + "content\0" + 输入内容 sha256)
//! ```
//!
//! 输入文件超过 [`CONTENT_HASH_LIMIT_BYTES`]（512MB）时退化为弱键（`size` +
//! 毫秒级 mtime，并在键中标注 `weak`），避免为命中缓存而读取超大文件。
//!
//! # 目录布局
//!
//! ```text
//! <workspace>/cache/<cache_key_hex>/
//!   meta.json                 # CacheMeta（版本、来源任务、文件清单、LRU 时刻）
//!   files/<node_id>/<name>    # 产物（优先硬链接，失败降级复制）
//! ```
//!
//! 命中任务由 daemon 侧构造为 `completed` 记录，产物通过
//! [`restore_into_task`] 硬链接/复制到新任务 `files/` 下；`cached_from`
//! 保留来源任务 id 以便任务详情标注。

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 内容哈希上限：超过后回退 `size + mtime` 弱键（512MB，handoff §5）
pub const CONTENT_HASH_LIMIT_BYTES: u64 = 512 * 1024 * 1024;

/// 缓存条目元数据格式版本（版本不一致视为未命中，绝不误读旧格式）
pub const CACHE_META_VERSION: u32 = 1;

const META_FILE: &str = "meta.json";
const FILES_DIR: &str = "files";
const KEY_DOMAIN: &str = "ep-direct-cache\0v1\0";

/// 输入文件指纹：内容强键，或超大文件的大小 + mtime 弱键
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputFingerprint {
    /// 文件内容 sha256（hex 小写）
    Content { sha256: String },
    /// 超大文件弱键：字节数 + 修改时间（epoch 毫秒，取不到时为 0）
    Weak { size: u64, mtime_ms: i64 },
}

impl InputFingerprint {
    /// 是否为弱键（超大文件回退）
    pub fn is_weak(&self) -> bool {
        matches!(self, Self::Weak { .. })
    }

    /// 参与键计算的规范化片段
    fn key_fragment(&self) -> String {
        match self {
            Self::Content { sha256 } => format!("content\0{sha256}"),
            Self::Weak { size, mtime_ms } => format!("weak\0{size}\0{mtime_ms}"),
        }
    }
}

/// 直跑缓存键
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    /// sha256 hex（64 字符小写）
    pub hex: String,
    /// 是否由弱键（超大文件 size + mtime）计算
    pub weak: bool,
}

/// 缓存条目中的一个产物文件
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFile {
    /// 产物节点 id（如 `output`）
    pub node_id: String,
    /// 缓存内文件名（已做路径穿越清洗）
    pub name: String,
    /// 字节数
    pub size: u64,
}

/// 缓存条目元数据（`meta.json`）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheMeta {
    pub version: u32,
    pub key: String,
    pub weak: bool,
    pub module_id: String,
    pub capability: String,
    /// 首次写入该缓存的来源任务 id（命中任务的 `cached_from`）
    pub source_task_id: String,
    /// 参数规范化 JSON 文本（审计/排障用）
    pub params: String,
    pub created_at_ms: i64,
    /// 最近命中时刻（epoch 毫秒，LRU 淘汰依据）
    pub last_used_ms: i64,
    /// 产物总字节数
    pub total_bytes: u64,
    /// 来源任务的节点顺序（命中记录据此构造详情）
    #[serde(default)]
    pub node_order: Vec<String>,
    #[serde(default)]
    pub files: Vec<CachedFile>,
}

/// 写入一个缓存条目的输入
#[derive(Debug, Clone, Copy)]
pub struct StoreRequest<'a> {
    pub key: &'a CacheKey,
    pub module_id: &'a str,
    pub capability: &'a str,
    pub params: &'a Value,
    pub source_task_id: &'a str,
    pub node_order: &'a [String],
    /// `(node_id, 产物源路径)`；源文件会被硬链接/复制进缓存目录
    pub files: &'a [(String, PathBuf)],
}

/// 递归规范化 JSON：对象键按字典序排序，数组保持原序，标量按 serde 序列化。
///
/// 保证参数顺序不同但语义相同的请求得到同一缓存键。
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (idx, key) in keys.into_iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                // serde_json 的字符串序列化必然是合法 JSON 字符串
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                if let Some(inner) = map.get(key) {
                    write_canonical(inner, out);
                } else {
                    out.push_str("null");
                }
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// 计算直跑缓存键（模块 + 能力 + 规范化参数 + 输入指纹）。
pub fn cache_key(
    module_id: &str,
    capability: &str,
    params: &Value,
    input: &InputFingerprint,
) -> CacheKey {
    let mut hasher = Sha256::new();
    hasher.update(KEY_DOMAIN.as_bytes());
    hasher.update(module_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(capability.as_bytes());
    hasher.update(b"\0");
    hasher.update(canonical_json(params).as_bytes());
    hasher.update(b"\0");
    hasher.update(input.key_fragment().as_bytes());
    CacheKey {
        hex: hex::encode(hasher.finalize()),
        weak: input.is_weak(),
    }
}

/// 计算输入文件指纹（≤512MB 读内容 sha256；超大文件回退 size + mtime）。
pub fn fingerprint_input(path: &Path) -> io::Result<InputFingerprint> {
    fingerprint_input_with_limit(path, CONTENT_HASH_LIMIT_BYTES)
}

/// 同 [`fingerprint_input`]，但内容哈希上限可配置（测试注入小阈值）。
pub fn fingerprint_input_with_limit(path: &Path, limit_bytes: u64) -> io::Result<InputFingerprint> {
    let meta = fs::metadata(path)?;
    let size = meta.len();
    if size > limit_bytes {
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        return Ok(InputFingerprint::Weak { size, mtime_ms });
    }
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(InputFingerprint::Content {
        sha256: hex::encode(hasher.finalize()),
    })
}

/// 当前时刻（epoch 毫秒）
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 缓存根目录下某键的条目目录
pub fn entry_dir(root: &Path, key: &CacheKey) -> PathBuf {
    root.join(&key.hex)
}

/// 读取条目元数据（不更新 LRU 时刻、不校验文件存在）。
pub fn read_meta(root: &Path, key: &CacheKey) -> Option<CacheMeta> {
    let entry = entry_dir(root, key);
    let text = fs::read_to_string(entry.join(META_FILE)).ok()?;
    let meta: CacheMeta = serde_json::from_str(&text).ok()?;
    (meta.version == CACHE_META_VERSION && meta.key == key.hex).then_some(meta)
}

/// 查询并“触达”缓存条目（更新 `last_used_ms`，供 LRU 使用）。
///
/// 元数据损坏、版本不符或任一产物文件缺失 → `None`（并清理坏条目），
/// 调用方按未命中处理。
pub fn lookup(root: &Path, key: &CacheKey) -> Option<CacheMeta> {
    let mut meta = read_meta(root, key)?;
    let entry = entry_dir(root, key);
    if !meta
        .files
        .iter()
        .all(|file| cached_file_path(&entry, file).is_file())
    {
        let _ = fs::remove_dir_all(&entry);
        return None;
    }
    meta.last_used_ms = now_ms();
    let _ = write_meta(&entry, &meta);
    Some(meta)
}

/// 写入一个直跑结果缓存条目，并按 `max_bytes` 执行 LRU 淘汰。
///
/// 已存在同键条目会被整体替换；`files` 为空视为可缓存结果为空的异常，
/// 返回 [`io::ErrorKind::InvalidInput`]（调用方跳过缓存即可）。
pub fn store(root: &Path, request: &StoreRequest<'_>, max_bytes: u64) -> io::Result<CacheMeta> {
    if request.files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no artifacts to cache",
        ));
    }
    let entry = entry_dir(root, request.key);
    if entry.exists() {
        fs::remove_dir_all(&entry)?;
    }
    let files_dir = entry.join(FILES_DIR);
    fs::create_dir_all(&files_dir)?;

    let mut cached_files = Vec::with_capacity(request.files.len());
    let mut total_bytes = 0u64;
    for (node_id, source) in request.files {
        let Some(raw_name) = source.file_name() else {
            continue;
        };
        let name = safe_component(&raw_name.to_string_lossy());
        let node_dir = files_dir.join(safe_component(node_id));
        fs::create_dir_all(&node_dir)?;
        let destination = node_dir.join(&name);
        materialize(source, &destination)?;
        let size = fs::metadata(&destination).map(|m| m.len()).unwrap_or(0);
        total_bytes = total_bytes.saturating_add(size);
        cached_files.push(CachedFile {
            node_id: node_id.clone(),
            name,
            size,
        });
    }
    if cached_files.is_empty() {
        let _ = fs::remove_dir_all(&entry);
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no valid artifact files to cache",
        ));
    }

    let now = now_ms();
    let meta = CacheMeta {
        version: CACHE_META_VERSION,
        key: request.key.hex.clone(),
        weak: request.key.weak,
        module_id: request.module_id.to_string(),
        capability: request.capability.to_string(),
        source_task_id: request.source_task_id.to_string(),
        params: canonical_json(request.params),
        created_at_ms: now,
        last_used_ms: now,
        total_bytes,
        node_order: request.node_order.to_vec(),
        files: cached_files,
    };
    write_meta(&entry, &meta)?;
    enforce_lru(root, max_bytes);
    Ok(meta)
}

/// 把缓存条目的产物硬链接/复制到新任务目录 `task_dir/files/<node_id>/`。
///
/// 返回 `(node_id, 任务内产物路径)` 列表；调用方据此填充
/// `TaskRecord.artifacts` / `served_artifacts`。
pub fn restore_into_task(
    root: &Path,
    key: &CacheKey,
    task_dir: &Path,
) -> io::Result<Vec<(String, PathBuf)>> {
    let entry = entry_dir(root, key);
    let meta = read_meta(root, key).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("cache entry {} missing", key.hex))
    })?;
    let mut restored = Vec::with_capacity(meta.files.len());
    for file in &meta.files {
        let source = cached_file_path(&entry, file);
        let dest_dir = task_dir.join(FILES_DIR).join(safe_component(&file.node_id));
        fs::create_dir_all(&dest_dir)?;
        let destination = dest_dir.join(safe_component(&file.name));
        materialize(&source, &destination)?;
        restored.push((file.node_id.clone(), destination));
    }
    Ok(restored)
}

/// LRU 淘汰：按 `last_used_ms` 从旧到新删除，直到缓存总量 ≤ `max_bytes`。
///
/// 返回被淘汰的字节数；`max_bytes == 0` 视为不限（不做任何删除）。
pub fn enforce_lru(root: &Path, max_bytes: u64) -> u64 {
    if max_bytes == 0 {
        return 0;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    let mut items: Vec<(i64, PathBuf, u64)> = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(text) = fs::read_to_string(dir.join(META_FILE)) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<CacheMeta>(&text) else {
            continue;
        };
        if meta.version != CACHE_META_VERSION {
            continue;
        }
        total = total.saturating_add(meta.total_bytes);
        items.push((meta.last_used_ms, dir, meta.total_bytes));
    }
    items.sort_by_key(|(last_used, _, _)| *last_used);
    let mut evicted = 0u64;
    for (_, dir, bytes) in items {
        if total <= max_bytes {
            break;
        }
        if fs::remove_dir_all(&dir).is_ok() {
            total = total.saturating_sub(bytes);
            evicted = evicted.saturating_add(bytes);
        }
    }
    evicted
}

/// 缓存目录当前占用字节数（按元数据累计，坏条目不计）。
pub fn cache_size(root: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let text = fs::read_to_string(entry.path().join(META_FILE)).ok()?;
            let meta: CacheMeta = serde_json::from_str(&text).ok()?;
            (meta.version == CACHE_META_VERSION).then_some(meta.total_bytes)
        })
        .fold(0u64, u64::saturating_add)
}

fn cached_file_path(entry: &Path, file: &CachedFile) -> PathBuf {
    entry
        .join(FILES_DIR)
        .join(safe_component(&file.node_id))
        .join(safe_component(&file.name))
}

fn write_meta(entry: &Path, meta: &CacheMeta) -> io::Result<()> {
    fs::create_dir_all(entry)?;
    let tmp = entry.join(format!("{META_FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(meta)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    fs::write(&tmp, bytes)?;
    let target = entry.join(META_FILE);
    if target.exists() {
        let _ = fs::remove_file(&target);
    }
    fs::rename(&tmp, &target)
}

/// 优先硬链接（零拷贝），失败降级为复制（跨盘/权限受限时仍可命中）。
fn materialize(source: &Path, destination: &Path) -> io::Result<()> {
    // 注意：这里必须用闭包而不是直接传 `fs::hard_link` 函数项——泛型函数项
    // 的生存期无法满足 `impl FnOnce(&Path, &Path)` 的 for<'a> 约束。
    materialize_with(source, destination, |src, dst| fs::hard_link(src, dst))
}

fn materialize_with(
    source: &Path,
    destination: &Path,
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    match link(source, destination) {
        Ok(()) => Ok(()),
        Err(link_err) => fs::copy(source, destination).map(|_| ()).map_err(|copy_err| {
            io::Error::new(
                copy_err.kind(),
                format!("hard link failed ({link_err}); copy fallback failed ({copy_err})"),
            )
        }),
    }
}

/// 把任意字符串清洗为单层安全文件名（防 `..`、路径分隔符与控制字符）。
fn safe_component(raw: &str) -> String {
    let base = raw
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|ch| if ch.is_control() { '_' } else { ch })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "artifact".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE_SEQ: AtomicU64 = AtomicU64::new(0);

    fn test_root(tag: &str) -> PathBuf {
        let seq = UNIQUE_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "ep_cache_{tag}_{}_{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn fingerprint(path: &Path) -> InputFingerprint {
        fingerprint_input(path).unwrap()
    }

    fn sample_files(dir: &Path, count: usize, size: usize) -> Vec<(String, PathBuf)> {
        (0..count)
            .map(|idx| {
                let path = dir.join(format!("artifact-{idx}.bin"));
                write_file(&path, &vec![b'a' + idx as u8; size]);
                ("output".to_string(), path)
            })
            .collect()
    }

    // ── 1. 规范化 JSON 与缓存键稳定性 ──────────────────────────────────────

    #[test]
    fn canonical_json_sorts_object_keys_and_preserves_arrays() {
        let left = json!({"b": 2, "a": {"y": 1, "x": 2}, "list": [3, 1]});
        let right = json!({"a": {"x": 2, "y": 1}, "b": 2, "list": [3, 1]});
        assert_eq!(canonical_json(&left), canonical_json(&right));
        assert_eq!(
            canonical_json(&left),
            r#"{"a":{"x":2,"y":1},"b":2,"list":[3,1]}"#
        );
        // 数组顺序是语义的一部分，不做排序
        assert_ne!(
            canonical_json(&json!({"list": [3, 1]})),
            canonical_json(&json!({"list": [1, 3]}))
        );
    }

    #[test]
    fn cache_key_is_stable_and_sensitive_to_semantic_changes() {
        let root = test_root("key");
        let input_path = root.join("input.wav");
        write_file(&input_path, b"audio-bytes");
        let input = fingerprint(&input_path);

        let params_a = json!({"language": "zh", "beam": {"width": 5, "top": 3}});
        let params_b = json!({"beam": {"top": 3, "width": 5}, "language": "zh"});
        let key_a = cache_key("whisper", "transcribe", &params_a, &input);
        let key_b = cache_key("whisper", "transcribe", &params_b, &input);
        assert_eq!(key_a, key_b);
        assert!(!key_a.weak);
        assert_eq!(key_a.hex.len(), 64);

        // 参数、模块、能力任一变化都必须换键
        let changed_params = cache_key(
            "whisper",
            "transcribe",
            &json!({"language": "en"}),
            &input,
        );
        assert_ne!(key_a.hex, changed_params.hex);
        assert_ne!(key_a.hex, cache_key("whisper", "other", &params_a, &input).hex);
        assert_ne!(key_a.hex, cache_key("other", "transcribe", &params_a, &input).hex);
        let _ = fs::remove_dir_all(&root);
    }

    // ── 2. 输入指纹（内容强键 / 超大文件弱键回退） ─────────────────────────

    #[test]
    fn fingerprint_small_file_uses_content_sha256() {
        let root = test_root("fp-small");
        let path = root.join("small.bin");
        write_file(&path, b"hello");
        let fp = fingerprint(&path);
        let expected = hex::encode(Sha256::digest(b"hello"));
        assert_eq!(
            fp,
            InputFingerprint::Content {
                sha256: expected
            }
        );
        assert!(!fp.is_weak());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fingerprint_oversize_file_falls_back_to_weak_key() {
        let root = test_root("fp-weak");
        let path = root.join("large.bin");
        write_file(&path, b"hello");
        let fp = fingerprint_input_with_limit(&path, 4).unwrap();
        match fp {
            InputFingerprint::Weak { size, .. } => assert_eq!(size, 5),
            other => panic!("expected weak fingerprint, got {other:?}"),
        }
        assert!(fp.is_weak());
        let key = cache_key("m", "c", &json!({}), &fp);
        assert!(key.weak);
        // 弱键与内容键必须不同，避免弱/强结果互相命中
        let strong = cache_key("m", "c", &json!({}), &InputFingerprint::Content {
            sha256: "0".repeat(64),
        });
        assert_ne!(key.hex, strong.hex);
        let _ = fs::remove_dir_all(&root);
    }

    // ── 3. 存储 / 查询 / 恢复 ──────────────────────────────────────────────

    #[test]
    fn store_lookup_and_restore_roundtrip() {
        let root = test_root("store");
        let artifacts = root.join("artifacts");
        let files = sample_files(&artifacts, 1, 16);
        let key = CacheKey {
            hex: "a".repeat(64),
            weak: false,
        };
        let params = json!({"a": 1, "b": [1, 2]});
        let node_order = vec!["input".to_string(), "run".to_string(), "output".to_string()];
        let request = StoreRequest {
            key: &key,
            module_id: "mock",
            capability: "transcribe",
            params: &params,
            source_task_id: "task-source",
            node_order: &node_order,
            files: &files,
        };
        let meta = store(&root, &request, 0).unwrap();
        assert_eq!(meta.version, CACHE_META_VERSION);
        assert_eq!(meta.source_task_id, "task-source");
        assert_eq!(meta.total_bytes, 16);
        assert_eq!(meta.node_order, node_order);

        let hit = lookup(&root, &key).expect("cache hit");
        assert_eq!(hit.files.len(), 1);
        assert_eq!(hit.files[0].node_id, "output");
        assert!(hit.last_used_ms >= meta.last_used_ms);

        let task_dir = root.join("tasks").join("task-hit");
        let restored = restore_into_task(&root, &key, &task_dir).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].0, "output");
        assert!(restored[0].1.starts_with(task_dir.join("files")));
        assert_eq!(fs::read(&restored[0].1).unwrap(), vec![b'a'; 16]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn store_without_artifacts_is_rejected() {
        let root = test_root("empty");
        let key = CacheKey {
            hex: "b".repeat(64),
            weak: false,
        };
        let request = StoreRequest {
            key: &key,
            module_id: "m",
            capability: "c",
            params: &json!({}),
            source_task_id: "t",
            node_order: &[],
            files: &[],
        };
        let err = store(&root, &request, 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!entry_dir(&root, &key).exists());
        let _ = fs::remove_dir_all(&root);
    }

    // ── 4. LRU 淘汰 ────────────────────────────────────────────────────────

    #[test]
    fn enforce_lru_evicts_least_recently_used_entries() {
        let root = test_root("lru");
        let artifacts = root.join("artifacts");
        let files = sample_files(&artifacts, 1, 100);
        let mut keys = Vec::new();
        for idx in 0..3 {
            let key = CacheKey {
                hex: format!("{idx}").repeat(64),
                weak: false,
            };
            let request = StoreRequest {
                key: &key,
                module_id: "m",
                capability: "c",
                params: &json!({"idx": idx}),
                source_task_id: "t",
                node_order: &[],
                files: &files,
            };
            store(&root, &request, 0).unwrap(); // 0 = 不限，先全部保留
            keys.push(key);
        }
        assert_eq!(cache_size(&root), 300);
        // 手工设置 LRU 时刻：keys[0] 最旧、keys[2] 最新
        for (idx, key) in keys.iter().enumerate() {
            let mut meta = read_meta(&root, key).unwrap();
            meta.last_used_ms = 1_000 + idx as i64;
            write_meta(&entry_dir(&root, key), &meta).unwrap();
        }
        let evicted = enforce_lru(&root, 200);
        assert_eq!(evicted, 100);
        assert!(read_meta(&root, &keys[0]).is_none());
        assert!(read_meta(&root, &keys[1]).is_some());
        assert!(read_meta(&root, &keys[2]).is_some());
        assert_eq!(cache_size(&root), 200);
        // max_bytes = 0 视为不限
        assert_eq!(enforce_lru(&root, 0), 0);
        let _ = fs::remove_dir_all(&root);
    }

    // ── 5. 硬链接失败降级复制 ──────────────────────────────────────────────

    #[test]
    fn materialize_falls_back_to_copy_when_hard_link_fails() {
        let root = test_root("fallback");
        let source = root.join("source.bin");
        write_file(&source, b"copy-me");
        let destination = root.join("nested").join("destination.bin");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        materialize_with(&source, &destination, |_, _| {
            Err(io::Error::other("simulated cross-device link failure"))
        })
        .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"copy-me");
        assert!(!destination.parent().unwrap().join("missing").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn safe_component_strips_path_traversal() {
        assert_eq!(safe_component("../../etc/passwd"), "passwd");
        assert_eq!(safe_component(".."), "artifact");
        assert_eq!(safe_component(""), "artifact");
        assert_eq!(safe_component("out.wav"), "out.wav");
    }
}
