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
//!
//! # 只读共享与污染防护（W2-F/F3）
//!
//! 产物入库与命中恢复都优先硬链接：缓存条目与任务产物共享同一 inode，任何
//! 一方就地截断/重写都会把污染内容扩散到所有命中任务，且内容寻址键无法检测。
//! 因此 [`store`] 在产物入缓存后将**缓存侧文件设为只读**（std 权限位，
//! Windows/Unix 通用，不依赖平台专有 API）。硬链接成功时来源任务的产物随
//! inode 共享只读属预期行为。**产物文件为只读共享，就地修改不允许**：模块
//! 对已入库产物就地写入会失败，这正是要阻止的污染路径；需要变更内容请写入
//! 新文件后整体替换，不要就地重写。[`restore_into_task`] 保持硬链接优先，
//! 命中任务侧的产物文件同样只读，属预期行为。

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

/// 孤儿条目回收宽限期（毫秒）：条目目录存在但 `meta.json` 缺失/损坏超过该
/// 时长才被 [`enforce_lru`] 整体回收，避免误删 `store` 进行中（`files/` 已建、
/// `meta.json` 尚未写）的活跃条目。
pub const ORPHAN_GRACE_MS: i64 = 60 * 60 * 1000;

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
///
/// 产物入库后缓存侧文件会被设为只读（见模块文档「只读共享与污染防护」）：
/// 缓存键是内容寻址的，任何一方就地重写都会静默污染所有命中任务。
///
/// 并发边界（F6，备查）：同键 `store` 的 remove→create 非原子。单任务内的
/// 写入经 daemon 侧 `spawn_blocking` 天然串行；跨任务同键并发时双方各自
/// 重建、后写者胜，最坏只造成一次缓存未命中，不会产生数据损坏——加锁的
/// 复杂度不抵收益，故仅在此备注，不做互斥。
pub fn store(root: &Path, request: &StoreRequest<'_>, max_bytes: u64) -> io::Result<CacheMeta> {
    if request.files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no artifacts to cache",
        ));
    }
    let entry = entry_dir(root, request.key);
    // 旧条目产物为只读共享；同键整体替换必须先删干净（现代 std 的
    // remove_dir_all 可删除只读文件，Windows/Unix 皆可）。
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
        // F3：入缓存立即设为只读。硬链接成功时与来源任务产物共享 inode，
        // 来源侧随之只读——就地截断/重写的污染路径就此关闭。
        set_readonly(&destination)?;
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
///
/// 缓存侧产物为只读（见模块文档「只读共享与污染防护」）：硬链接成功时任务
/// 侧文件与缓存共享 inode，同样只读——这是防污染语义的一部分，属预期行为；
/// 硬链接失败降级复制时，目标文件也带只读属性（Windows 复制保留只读位，
/// Unix 复制权限位）。就地重写任务产物会失败，需要变更内容请写新文件。
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
/// 同时做**孤儿条目回收**（W2-F/F4）：`store` 先建 `files/` 后写 `meta.json`，
/// 中间崩溃会留下无有效 `meta.json` 的孤儿目录——它们对 LRU/`cache_size`
/// 不可见、永不淘汰。因此扫描时对「`meta.json` 缺失 / JSON 损坏 / 版本不
/// 识别，且目录 mtime 早于 [`ORPHAN_GRACE_MS`] 宽限期」的条目整体
/// `remove_dir_all` 回收；宽限期内保留，避免误删正在写入的活跃条目。
///
/// `max_bytes == 0` 视为不限容量：跳过 LRU 淘汰，但孤儿回收照常执行（孤儿
/// 对容量统计不可见，不限容量时若不回收则永不回收）。回收不计入返回值
/// （无元数据可查，字节数未知）。
///
/// 返回按 LRU 淘汰的字节数。
pub fn enforce_lru(root: &Path, max_bytes: u64) -> u64 {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    let now = now_ms();
    let mut items: Vec<(i64, PathBuf, u64)> = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        match meta_from_entry_dir(&dir) {
            Some(meta) => {
                total = total.saturating_add(meta.total_bytes);
                items.push((meta.last_used_ms, dir, meta.total_bytes));
            }
            // 无有效元数据 → 孤儿候选；超过宽限期才整体回收
            None => {
                if entry_dir_is_stale(&dir, now) {
                    let _ = fs::remove_dir_all(&dir);
                }
            }
        }
    }
    if max_bytes == 0 {
        return 0;
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
        .filter_map(|entry| entry.path().is_dir().then_some(entry.path()))
        .filter_map(|dir| meta_from_entry_dir(&dir).map(|meta| meta.total_bytes))
        .fold(0u64, u64::saturating_add)
}

/// 读取条目目录内的 `meta.json` 并校验格式版本；缺失 / 损坏 / 版本不识别
/// 一律返回 `None`（enforce_lru 视为孤儿候选，cache_size 不计）。
fn meta_from_entry_dir(dir: &Path) -> Option<CacheMeta> {
    let text = fs::read_to_string(dir.join(META_FILE)).ok()?;
    let meta: CacheMeta = serde_json::from_str(&text).ok()?;
    (meta.version == CACHE_META_VERSION).then_some(meta)
}

/// 条目目录 mtime 是否早于孤儿回收宽限期（取不到 mtime 时保守视为不回收）。
fn entry_dir_is_stale(dir: &Path, now_ms_value: i64) -> bool {
    let cutoff = (now_ms_value - ORPHAN_GRACE_MS).max(0) as u128;
    fs::metadata(dir)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .is_some_and(|elapsed| elapsed.as_millis() <= cutoff)
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

/// 把文件设为只读（跨平台 std 权限位：Windows 映射 FILE_ATTRIBUTE_READONLY，
/// Unix 清除全部写权限位；不使用任何平台专有 API）。
fn set_readonly(path: &Path) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms)
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

    // ── 6. 缓存污染防护：只读共享（W2-F/F3）───────────────────────────────

    /// 在 `root` 下写入单文件缓存条目（源文件放独立 `slot` 子目录，避免同键
    /// 重测时与上一轮的只读源文件冲突）。返回 `(键, 缓存侧产物路径)`。
    fn store_single(root: &Path, tag: char, slot: &str, bytes: &[u8]) -> (CacheKey, PathBuf) {
        let key = CacheKey {
            hex: tag.to_string().repeat(64),
            weak: false,
        };
        let src_dir = root.join("src").join(slot);
        let src = src_dir.join("artifact-0.bin");
        write_file(&src, bytes);
        let node_order = vec!["output".to_string()];
        let files = vec![("output".to_string(), src)];
        let request = StoreRequest {
            key: &key,
            module_id: "m",
            capability: "c",
            params: &json!({}),
            source_task_id: "t",
            node_order: &node_order,
            files: &files,
        };
        store(root, &request, 0).unwrap();
        let cache_file = entry_dir(root, &key)
            .join(FILES_DIR)
            .join("output")
            .join("artifact-0.bin");
        (key, cache_file)
    }

    #[test]
    fn stored_cache_artifacts_are_readonly_and_reject_inplace_rewrite() {
        let root = test_root("readonly");
        let payload = vec![7u8; 32];
        let (_key, cache_file) = store_single(&root, 'c', "first", &payload);

        assert!(cache_file.is_file());
        assert!(
            fs::metadata(&cache_file).unwrap().permissions().readonly(),
            "缓存侧产物必须为只读"
        );

        // F3 污染路径：对缓存条目就地截断/重写必须被权限拒绝
        let err = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&cache_file)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // 内容保持不变
        assert_eq!(fs::read(&cache_file).unwrap(), payload);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restored_task_artifacts_are_readonly() {
        let root = test_root("restore-readonly");
        let payload = vec![9u8; 24];
        let (key, _) = store_single(&root, 'd', "src", &payload);

        let task_dir = root.join("tasks").join("task-hit");
        let restored = restore_into_task(&root, &key, &task_dir).unwrap();
        assert_eq!(restored.len(), 1);
        let task_file = &restored[0].1;
        assert!(
            fs::metadata(task_file).unwrap().permissions().readonly(),
            "任务侧产物只读属预期（硬链接共享 inode / 复制保留只读位）"
        );
        let err = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(task_file)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(task_file).unwrap(), payload);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn store_replaces_existing_entry_despite_readonly_artifacts() {
        let root = test_root("replace-readonly");
        let (key, first_file) = store_single(&root, 'e', "v1", b"first-version");
        assert_eq!(fs::read(&first_file).unwrap(), b"first-version");

        // 同键整体替换：旧条目的只读产物必须能被清掉（无 PermissionDenied）
        let (key2, second_file) = store_single(&root, 'e', "v2", b"second-version!!");
        assert_eq!(key2, key);
        assert_eq!(fs::read(&second_file).unwrap(), b"second-version!!");
        assert!(
            fs::metadata(&second_file).unwrap().permissions().readonly(),
            "替换后的新产物仍须为只读"
        );
        assert!(read_meta(&root, &key).is_some());
        assert_eq!(lookup(&root, &key).unwrap().total_bytes, 16);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn materialize_overwrites_existing_readonly_destination() {
        // 重复恢复同一缓存条目到同一任务目录时，旧目标文件是只读的：
        // 硬链接失败的降级复制路径也必须能覆盖（现代 std 可删除只读文件的
        // 回归哨兵；依赖此假设的实现一旦变化，本测试会先红）。
        let root = test_root("mat-readonly-dest");
        let source = root.join("source.bin");
        write_file(&source, b"new-content");
        let dest = root.join("dest").join("artifact.bin");
        write_file(&dest, b"old");
        let mut perms = fs::metadata(&dest).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&dest, perms).unwrap();

        materialize_with(&source, &dest, |_, _| {
            Err(io::Error::other("simulated cross-device link failure"))
        })
        .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new-content");
        let _ = fs::remove_dir_all(&root);
    }

    // ── 7. 孤儿条目回收（W2-F/F4）──────────────────────────────────────────

    /// 把文件/目录的 mtime 回拨 `delta_ms`（测试孤儿回收宽限期用）。
    #[cfg(windows)]
    fn open_for_mtime_edit(path: &Path) -> io::Result<fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES(0x0100) + FILE_FLAG_BACKUP_SEMANTICS(0x0200_0000)：
        // 以最小权限打开目录句柄，供 set_modified 回拨目录 mtime。
        fs::OpenOptions::new()
            .access_mode(0x0000_0100)
            .custom_flags(0x0200_0000)
            .open(path)
    }

    #[cfg(not(windows))]
    fn open_for_mtime_edit(path: &Path) -> io::Result<fs::File> {
        fs::File::open(path)
    }

    fn backdate_mtime(path: &Path, delta_ms: i64) {
        let stamp = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_millis((now_ms() - delta_ms).max(0) as u64);
        open_for_mtime_edit(path)
            .expect("open path for mtime edit")
            .set_modified(stamp)
            .expect("set_modified");
    }

    #[test]
    fn enforce_lru_reclaims_stale_orphans_and_spares_fresh_or_valid_entries() {
        let root = test_root("orphan");

        // 正常条目（有有效 meta）——即使目录 mtime 超过宽限期也不被孤儿回收
        let (valid_key, _) = store_single(&root, 'f', "valid", b"valid-entry");
        backdate_mtime(&entry_dir(&root, &valid_key), ORPHAN_GRACE_MS * 2);

        // 新鲜孤儿：无 meta.json、mtime 在宽限期内（模拟 store 进行中的条目）
        let fresh_orphan = entry_dir(
            &root,
            &CacheKey {
                hex: "1".repeat(64),
                weak: false,
            },
        );
        write_file(
            &fresh_orphan.join(FILES_DIR).join("output").join("a.bin"),
            b"fresh",
        );
        backdate_mtime(&fresh_orphan, ORPHAN_GRACE_MS / 2);

        // 陈旧孤儿：无 meta.json、mtime 超过宽限期、且产物只读（模拟崩溃于
        // set_readonly 之后、write_meta 之前）→ 必须整体回收
        let stale_orphan = entry_dir(
            &root,
            &CacheKey {
                hex: "2".repeat(64),
                weak: false,
            },
        );
        let stale_file = stale_orphan.join(FILES_DIR).join("output").join("a.bin");
        write_file(&stale_file, b"stale");
        set_readonly(&stale_file).unwrap();
        backdate_mtime(&stale_orphan, ORPHAN_GRACE_MS * 2);

        // 损坏 meta 且超期 → 必须整体回收
        let corrupt_orphan = entry_dir(
            &root,
            &CacheKey {
                hex: "3".repeat(64),
                weak: false,
            },
        );
        write_file(&corrupt_orphan.join(META_FILE), b"{not json");
        backdate_mtime(&corrupt_orphan, ORPHAN_GRACE_MS * 2);

        // max_bytes = 0（不限容量）不做 LRU 淘汰，但孤儿回收照常执行
        let evicted = enforce_lru(&root, 0);
        assert_eq!(evicted, 0, "孤儿回收无元数据可查，不计入淘汰字节");
        assert!(
            read_meta(&root, &valid_key).is_some(),
            "有 meta 的正常条目永不被孤儿回收误删"
        );
        assert!(
            fresh_orphan.is_dir(),
            "宽限期内的孤儿（进行中的 store）不回收"
        );
        assert!(!stale_orphan.exists(), "超期孤儿必须整体回收");
        assert!(!corrupt_orphan.exists(), "超期损坏条目必须整体回收");
        assert_eq!(cache_size(&root), 11, "cache_size 只统计有效条目");

        // 孤儿回收后 LRU 淘汰不受影响：有效条目容量充足时不被淘汰
        assert_eq!(enforce_lru(&root, 1024), 0);
        assert!(read_meta(&root, &valid_key).is_some());
        let _ = fs::remove_dir_all(&root);
    }
}
