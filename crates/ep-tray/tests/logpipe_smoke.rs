//! GT-4 LogPipe 真机冒烟测试（`#[ignore]`——需要现场条件，常规 `cargo test` 跳过）。
//!
//! 前置条件（2026-11-06 现场口径）：
//! - 本机 daemon 活着：`ws://127.0.0.1:9800/ws/logs` 可握手；
//! - 仓根 `runtime/logs/` 有真实 `daemon.log`（tracing ANSI 真迹，2026-08 生成）
//!   与 `events-2026-*.jsonl`。
//!
//! 注意：现场 daemon（dist/tray-demo，PID 24276）stdout 未做文件重定向——
//! `runtime/logs/daemon.log` 是历史真迹而非活文件，故 FileTailSource 用
//! [`TailStart::Beginning`] 读全量验证「读到真实行」；GT-5 接线后日常用
//! 默认 `TailStart::End`。本测试只读不写，绝不触碰在跑 daemon。
//!
//! 实跑：`cargo test -p ep-tray --test logpipe_smoke -- --ignored --nocapture`

#![cfg(windows)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ep_tray::platform::logpipe::file_tail::TailStart;
use ep_tray::platform::logpipe::{LogPipe, LogPipeConfig, LogSource};

/// root = 仓根（CARGO_MANIFEST_DIR = crates/ep-tray → ../.. = 仓根）
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

#[test]
#[ignore]
fn logpipe_smoke_real_daemon_15s() {
    let cfg = LogPipeConfig {
        root: repo_root(),
        daemon_port: 9800,
        ring_cap: 5000,
        tail_start: TailStart::Beginning,
    };
    let mut handle = LogPipe::spawn(cfg);

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut total = 0usize;
    let mut last_status_line = String::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        let records = handle.drain_new();
        if !records.is_empty() {
            total += records.len();
        }
        let st = handle.ws_status();
        last_status_line = format!(
            "ws={:?} attempts={} bad_frames={} ring_len={} dropped={} jsonl_bad={}",
            st.state,
            st.attempts,
            st.bad_frames,
            handle.ring_len(),
            handle.drop_count(),
            handle.jsonl_bad_lines()
        );
        println!("[{:>4.1}s] +{:<3} new | {}", (15.0 - (deadline - Instant::now()).as_secs_f32()), records.len(), last_status_line);
    }

    println!("=== 冒烟终态 ===\n{last_status_line}\n累计入环 {total} 行");

    // ── 断言 1：WsSource 握手成功（daemon 活着，101 升级后 recv 通道建立）──
    let st = handle.ws_status();
    println!("WsStatus: {st:?}");
    assert_eq!(
        st.state,
        ep_tray::platform::logpipe::ws_source::WsConnState::Connected,
        "15s 内应与 ws://127.0.0.1:9800/ws/logs 握手成功"
    );
    assert!(st.last_connected.is_some());
    assert_eq!(st.bad_frames, 0, "daemon 发出的帧应全部可解析");

    // ── 断言 2：FileTailSource 读到 daemon.log 真实行（ANSI 已剥离）──
    let mut handle2 = LogPipe::spawn(LogPipeConfig {
        root: repo_root(),
        daemon_port: 9800,
        ring_cap: 5000,
        tail_start: TailStart::Beginning,
    });
    std::thread::sleep(Duration::from_millis(1200)); // tail 首轮 300ms + 余量
    let records = handle2.drain_new();
    let daemon: Vec<&ep_tray::platform::logpipe::LogRecord> = records
        .iter()
        .filter(|r| r.source == LogSource::Daemon)
        .collect();
    println!(
        "FileTail: 读到 {} 条 daemon 行，样例：{:?}",
        daemon.len(),
        daemon.first().map(|r| &r.line)
    );
    assert!(
        daemon.len() >= 10,
        "应从真实 daemon.log（~450 行 tracing 真迹）读到足量行"
    );
    assert!(
        daemon.iter().all(|r| !r.line.contains('\x1b')),
        "所有行应已剥离 ANSI"
    );
    assert!(
        daemon.iter().any(|r| r.level.is_some()),
        "真实 tracing 行应可解析出级别"
    );

    // ── 断言 3：EventsJsonlSource 读到历史事件行 ──
    let events: Vec<&ep_tray::platform::logpipe::LogRecord> = records
        .iter()
        .filter(|r| r.source == LogSource::Event)
        .collect();
    println!(
        "EventsJsonl: 读到 {} 条事件，样例：{:?}",
        events.len(),
        events.first().map(|r| &r.line)
    );
    assert!(
        !events.is_empty(),
        "应从 events-2026-09.jsonl 启动载入最近 200 行"
    );
    assert_eq!(handle2.jsonl_bad_lines(), 0, "真文件无坏行");
    println!("=== 冒烟通过 ===");
}
