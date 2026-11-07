//! 日志导出（GT-6）：daemon 日志/统一事件/任务注册表打包 zip——journalctl
//! 导出的 Windows 等价物。
//!
//! 数据源**文件直读**（GT-5 报告 §4 裁决：ring/显示缓冲是实时流视图且有上限，
//! 导出要全量历史）。清单走白名单精确匹配——runtime/logs、runtime/tasks 里
//! 混有测试垃圾（*.mp4、fanout*.json、*.py），整目录扫会把垃圾打进包。
//!
//! 红线：config/app.toml 含密钥（[server].token 为真实鉴权面），默认不导出、
//! 显式勾选时逐行掩码（T-24/T-52 精神）；单文件 200MB 护栏（防呆不防常态）。

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

/// events jsonl 导出范围
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventsRange {
    /// 全部 events-*.jsonl（按月分文件，全打）
    All,
    /// 仅当前月（events-YYYY-MM.jsonl）
    CurrentMonth,
}

/// 导出选项（UI 勾选面）
#[derive(Debug, Clone, Copy)]
pub struct ExportOptions {
    pub events: EventsRange,
    /// runtime/tasks/task-*.json 任务注册表（默认开）
    pub include_tasks: bool,
    /// config/app.toml（默认关；true 时掩码后打包）
    pub include_config: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            events: EventsRange::All,
            include_tasks: true,
            include_config: false,
        }
    }
}

/// 单个导出条目
#[derive(Debug, Clone)]
pub struct ExportItem {
    /// zip 内相对路径
    pub rel: String,
    /// 磁盘来源
    pub src: PathBuf,
    /// 读取时是否掩码（仅 config）
    pub mask: bool,
}

/// 导出结果报告
#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub out_path: PathBuf,
    pub files: usize,
    pub total_bytes: u64,
    /// 超护栏被跳过的文件
    pub skipped: Vec<String>,
    /// 掩码命中的行数（config 勾选时）
    pub masked_lines: usize,
}

/// 单文件护栏：超过即跳过（防呆——正常日志/事件/任务注册表远小于此）
pub const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;

/// 当前本地月份 `YYYY-MM`（events 文件名过滤用；时区 API 失败降级 UTC，
/// 与 logs.rs 本地时间显示同一 API 链——不引 chrono）。
pub fn current_month_local() -> String {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{
        FileTimeToSystemTime, GetTimeZoneInformation, SystemTimeToTzSpecificLocalTime,
        TIME_ZONE_ID_INVALID, TIME_ZONE_INFORMATION,
    };
    const EPOCH_DIFF_100NS: u64 = 11_644_473_600 * 10_000_000;
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ft_100ns = d.as_secs() * 10_000_000 + (d.subsec_nanos() / 100) as u64 + EPOCH_DIFF_100NS;
    let ft = FILETIME {
        dwLowDateTime: (ft_100ns & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ft_100ns >> 32) as u32,
    };
    let mut utc = SYSTEMTIME {
        wYear: 0, wMonth: 0, wDayOfWeek: 0, wDay: 0, wHour: 0, wMinute: 0, wSecond: 0,
        wMilliseconds: 0,
    };
    // 关键：FILETIME 先转 UTC SYSTEMTIME，再送时区转换（空 SYSTEMTIME 算出垃圾日期）
    unsafe { FileTimeToSystemTime(&ft, &mut utc) };
    let mut tzi = TIME_ZONE_INFORMATION::default();
    let mut local = SYSTEMTIME { wYear: 0, wMonth: 0, wDayOfWeek: 0, wDay: 0, wHour: 0, wMinute: 0, wSecond: 0, wMilliseconds: 0 };
    let tz_ok = unsafe { GetTimeZoneInformation(&mut tzi) } != TIME_ZONE_ID_INVALID
        && unsafe { SystemTimeToTzSpecificLocalTime(&tzi, &utc, &mut local) } != 0;
    if tz_ok {
        return format!("{:04}-{:02}", local.wYear, local.wMonth);
    }
    // 时区 API 失败降级 UTC 日期（与 logs.rs 显示面同语义），不空串（空串会让
    // CurrentMonth 退化成不过滤——All 语义，违背用户选择）
    format!("{:04}-{:02}", utc.wYear, utc.wMonth)
}

/// 收集导出清单（纯逻辑：只读元数据+过滤，不开文件内容）。
pub fn collect_export_set(root: &Path, opts: &ExportOptions, now_month: &str) -> Vec<ExportItem> {
    let mut items = Vec::new();
    let push = |items: &mut Vec<ExportItem>, rel: String, src: PathBuf, mask: bool| {
        if src.is_file() {
            items.push(ExportItem { rel, src, mask });
        }
    };

    // ① daemon.log（tracing 主日志）
    push(
        &mut items,
        "logs/daemon.log".into(),
        root.join("runtime/logs/daemon.log"),
        false,
    );

    // ② events-*.jsonl（统一事件日志按月分文件）
    if let Ok(rd) = std::fs::read_dir(root.join("runtime/logs")) {
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("events-") && n.ends_with(".jsonl"))
                    .unwrap_or(false)
            })
            .collect();
        found.sort();
        for p in found {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if opts.events == EventsRange::CurrentMonth && !name.contains(now_month) {
                continue;
            }
            push(&mut items, format!("logs/{name}"), p, false);
        }
    }

    // ③ 任务注册表 task-*.json（runtime/tasks/ 下同名测试垃圾文件不收）
    if opts.include_tasks {
        if let Ok(rd) = std::fs::read_dir(root.join("runtime/tasks")) {
            let mut found: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("task-") && n.ends_with(".json"))
                        .unwrap_or(false)
                })
                .collect();
            found.sort();
            for p in found {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                push(&mut items, format!("tasks/{name}"), p, false);
            }
        }
    }

    // ④ config/app.toml（默认不导出；导出必掩码）
    if opts.include_config {
        push(
            &mut items,
            "config/app.toml".into(),
            root.join("config/app.toml"),
            true,
        );
    }

    // ⑤ VERSION.txt（构建指纹，排障用）
    push(&mut items, "VERSION.txt".into(), root.join("VERSION.txt"), false);

    items
}

/// 掩码单行：命中敏感键名（token/secret/password/api_key/key，大小写不敏感）
/// 且形如 `key = "value"` 的行 → 值替换为 ***MASKED***。
/// 判据刻意保守：只动双引号标量值，数组/表结构原样保留。
pub fn mask_toml_line(line: &str) -> Option<String> {
    let lowered = line.to_ascii_lowercase();
    let sensitive = ["token", "secret", "password", "api_key"];
    if !sensitive.iter().any(|k| lowered.contains(k)) {
        return None;
    }
    // 形如 `xxx = "yyy"`（允许前后空白；值内不含未转义引号的常规形态）
    let eq = line.find('=')?;
    let (key_part, val_part) = line.split_at(eq);
    let val_trim = val_part[1..].trim_start();
    let val = val_trim.strip_prefix('"')?;
    let value_end = val.find('"')?; // 常规 TOML 基本字符串：下一个裸引号即结尾
    let after = &val[value_end..];
    let _ = key_part;
    let masked = format!(
        "{}= \"***MASKED***\"{}",
        key_part,
        // after 以 `"` 开头，其后是行尾注释/空白
        after
    );
    // 保留行尾注释
    let line_end = after.find('#').map(|i| &after[i..]).unwrap_or("");
    let mut out = masked;
    if !line_end.is_empty() {
        // 去掉 after 里已经并入的引号，改为引号 + 原注释
        out = format!("{}= \"***MASKED***\" {}", key_part, line_end);
    }
    Some(out)
}

/// 对整个 TOML 文本掩码，返回 (掩码后文本, 命中行数)。
pub fn mask_toml_text(text: &str) -> (String, usize) {
    let mut masked = 0usize;
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        match mask_toml_line(line) {
            Some(l) => {
                masked += 1;
                out.push_str(&l);
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    (out, masked)
}

/// 打包 zip（阻塞 IO——调用方保证在后台线程）。返回结果报告。
pub fn build_zip(items: &[ExportItem], out_path: &Path) -> std::io::Result<ExportOutcome> {
    use zip::ZipWriter;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(out_path)?;
    let mut zw = ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut files = 0usize;
    let mut total = 0u64;
    let mut skipped = Vec::new();
    let mut masked_lines = 0usize;

    for item in items {
        let meta = match std::fs::metadata(&item.src) {
            Ok(m) => m,
            Err(_) => {
                skipped.push(format!("{}（读取元数据失败）", item.rel));
                continue;
            }
        };
        if meta.len() > MAX_FILE_BYTES {
            skipped.push(format!(
                "{}（{:.0}MB 超护栏）",
                item.rel,
                meta.len() as f64 / 1048576.0
            ));
            continue;
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        match std::fs::File::open(&item.src).and_then(|mut f| f.read_to_end(&mut bytes)) {
            Ok(_) => {}
            Err(e) => {
                skipped.push(format!("{}（打开失败: {e}）", item.rel));
                continue;
            }
        }
        if item.mask {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let (masked_text, n) = mask_toml_text(&text);
            bytes = masked_text.into_bytes();
            masked_lines += n;
        }
        total += bytes.len() as u64;
        zw.start_file(item.rel.as_str(), options)
            .map_err(|e| std::io::Error::other(format!("zip start_file {}: {e}", item.rel)))?;
        zw.write_all(&bytes)?;
        files += 1;
    }
    zw.finish()
        .map_err(|e| std::io::Error::other(format!("zip finish: {e}")))?;

    Ok(ExportOutcome {
        out_path: out_path.to_path_buf(),
        files,
        total_bytes: total,
        skipped,
        masked_lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn collect_whitelist_exact() {
        let dir = std::env::temp_dir().join(format!("ep-tray-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir.join("runtime/logs/daemon.log"), "log");
        write(&dir.join("runtime/logs/events-2026-10.jsonl"), "e1");
        write(&dir.join("runtime/logs/events-2026-11.jsonl"), "e2");
        // 测试垃圾：不得入清单
        write(&dir.join("runtime/logs/timed_src.mp4"), "junk");
        write(&dir.join("runtime/logs/fanout.json"), "junk");
        write(&dir.join("runtime/tasks/task-a.json"), "{}");
        write(&dir.join("runtime/tasks/check_x.py"), "junk");
        write(&dir.join("config/app.toml"), "token = \"s\"");
        write(&dir.join("VERSION.txt"), "v");

        let all = collect_export_set(
            &dir,
            &ExportOptions::default(),
            "2026-11",
        );
        let rels: Vec<&str> = all.iter().map(|i| i.rel.as_str()).collect();
        assert_eq!(
            rels,
            vec![
                "logs/daemon.log",
                "logs/events-2026-10.jsonl",
                "logs/events-2026-11.jsonl",
                "tasks/task-a.json",
                "VERSION.txt",
            ]
        );

        let cur = collect_export_set(
            &dir,
            &ExportOptions { events: EventsRange::CurrentMonth, ..Default::default() },
            "2026-11",
        );
        assert_eq!(cur.iter().filter(|i| i.rel.starts_with("logs/events")).count(), 1);

        let cfg = collect_export_set(
            &dir,
            &ExportOptions { include_config: true, ..Default::default() },
            "2026-11",
        );
        assert!(cfg.iter().any(|i| i.rel == "config/app.toml" && i.mask));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mask_covers_server_token_and_leaves_others() {
        let (out, n) = mask_toml_text(
            "[server]\ntoken = \"sk-secret-123\"\nport = 9800\n[api]\napi_key = \"abc\" # comment\nname = \"x\"\n",
        );
        assert_eq!(n, 2);
        assert!(out.contains("token = \"***MASKED***\""));
        assert!(out.contains("api_key = \"***MASKED***\" # comment"));
        assert!(out.contains("port = 9800"));
        assert!(!out.contains("sk-secret"));
    }

    #[test]
    fn zip_roundtrip_and_mask() {
        let dir = std::env::temp_dir().join(format!("ep-tray-export-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir.join("runtime/logs/daemon.log"), "hello log");
        write(&dir.join("config/app.toml"), "[server]\ntoken = \"sk-x\"\n");
        let items = vec![
            ExportItem { rel: "logs/daemon.log".into(), src: dir.join("runtime/logs/daemon.log"), mask: false },
            ExportItem { rel: "config/app.toml".into(), src: dir.join("config/app.toml"), mask: true },
        ];
        let out_path = dir.join("out.zip");
        let outcome = build_zip(&items, &out_path).unwrap();
        assert_eq!(outcome.files, 2);
        assert_eq!(outcome.masked_lines, 1);

        // 读回校验
        let f = std::fs::File::open(&out_path).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut log = String::new();
        use std::io::Read;
        z.by_name("logs/daemon.log").unwrap().read_to_string(&mut log).unwrap();
        assert_eq!(log, "hello log");
        let mut cfg = String::new();
        z.by_name("config/app.toml").unwrap().read_to_string(&mut cfg).unwrap();
        assert!(cfg.contains("***MASKED***"));
        assert!(!cfg.contains("sk-x"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 真机冒烟（#[ignore]）：对本仓 root 实跑 collect+build_zip，
/// `cargo test -p ep-tray -- --ignored` 留证（GT-6 验收项）。
#[test]
#[ignore]
fn export_real_root_smoke() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf();
    let items = collect_export_set(&root, &ExportOptions::default(), &current_month_local());
    assert!(items.iter().any(|i| i.rel == "logs/daemon.log"), "daemon.log 必须在清单");
    assert!(items.iter().any(|i| i.rel.starts_with("logs/events-")), "events jsonl 必须在清单");
    assert!(items.iter().any(|i| i.rel.starts_with("tasks/")), "任务注册表必须在清单");
    assert!(!items.iter().any(|i| i.rel.ends_with(".mp4")), "垃圾文件不得入清单");
    let out = std::env::temp_dir().join("ep-tray-gt6-smoke.zip");
    let outcome = build_zip(&items, &out).unwrap();
    eprintln!("smoke: files={} bytes={} skipped={:?}", outcome.files, outcome.total_bytes, outcome.skipped);
    assert!(outcome.files >= 3);
    assert!(outcome.skipped.is_empty());
    let _ = std::fs::remove_file(&out);
}
