//! 端口管理器 — 从配置范围内分配/释放模块端口

use std::collections::HashMap;

use anyhow::{bail, Result};
use tracing::debug;

/// 端口管理器：在 [range_start, range_end] 范围内为模块分配唯一端口
pub struct PortManager {
    range_start: u16,
    range_end: u16,
    /// module_id → 已分配端口
    allocations: HashMap<String, u16>,
}

impl PortManager {
    /// 创建端口管理器，端口范围为 [range_start, range_end]（含两端）
    pub fn new(range_start: u16, range_end: u16) -> Self {
        Self {
            range_start,
            range_end,
            allocations: HashMap::new(),
        }
    }

    /// 为模块分配下一个可用端口。
    ///
    /// 如果模块已持有端口则直接返回；否则从范围起始处顺序查找第一个
    /// 同时满足以下两个条件的端口：
    ///
    /// - 进程内未被声明占用（未分配给其他模块，见内部 HashMap）；
    /// - OS 层面未被实际占用：依次在 `0.0.0.0` 与 `127.0.0.1` 上绑定
    ///   临时监听器探测（见 [`os_port_free`]，Windows 绑定共存语义要求
    ///   双地址探测），任一探测失败即视为被占用（例如残留的孤儿模块
    ///   进程仍监听着该端口），跳过该候选继续扫描下一个。
    ///
    /// 范围耗尽时返回错误。
    ///
    /// 注意（TOCTOU）：OS 探测与实际监听之间存在竞争窗口——探测用临时
    /// 监听器释放端口后、模块进程真正绑定前，该端口仍可能被其他进程
    /// 抢占。此竞态可接受；若真的发生冲突，会在模块绑定失败时暴露。
    pub fn allocate(&mut self, module_id: &str) -> Result<u16> {
        // 已分配则直接返回：模块运行中其端口被监听属正常状态，OS 探测会把
        // 正常运行的模块误判为"占用"而错误换端口（孤儿进程端口冲突由
        // process.rs teardown 的进程树回收兜底，见 P0 修复）
        if let Some(&port) = self.allocations.get(module_id) {
            debug!(module_id, port, "port already allocated, returning existing");
            return Ok(port);
        }

        // 收集已占用端口集合
        let used: std::collections::HashSet<u16> = self.allocations.values().copied().collect();

        for port in self.range_start..=self.range_end {
            if used.contains(&port) {
                continue;
            }
            if !os_port_free(port) {
                debug!(port, "port occupied at OS level, skipping");
                continue;
            }
            self.allocations.insert(module_id.to_string(), port);
            debug!(module_id, port, "allocated port");
            return Ok(port);
        }

        bail!(
            "port range [{}, {}] exhausted, no available port for module '{}'",
            self.range_start,
            self.range_end,
            module_id
        )
    }

    /// 释放模块占用的端口
    pub fn release(&mut self, module_id: &str) {
        if let Some(port) = self.allocations.remove(module_id) {
            debug!(module_id, port, "released port");
        }
    }

    /// 查询模块当前分配的端口
    pub fn get_port(&self, module_id: &str) -> Option<u16> {
        self.allocations.get(module_id).copied()
    }

    /// 检查端口是否在管理范围内且未被分配、OS 层面空闲（与 allocate 语义对齐）
    pub fn is_available(&self, port: u16) -> bool {
        if port < self.range_start || port > self.range_end {
            return false;
        }
        !self.allocations.values().any(|&p| p == port) && os_port_free(port)
    }

    /// 当前已分配的端口数量
    pub fn allocated_count(&self) -> usize {
        self.allocations.len()
    }
}

/// 探测端口当前在 OS 层面是否空闲：依次尝试绑定 `0.0.0.0`（IPv4 通配）
/// 与 `127.0.0.1`（回环）上的临时监听器，**全部成功**才算空闲
/// （临时监听器用完即释放），任一地址绑定失败即判占用。
///
/// 双地址探测的依据（Windows 绑定语义，已实机验证）：端口已被某进程以
/// `0.0.0.0:port` 监听时，再 bind `127.0.0.1:port` **仍会成功**——除非设置
/// SO_EXCLUSIVEADDRUSE，Windows 允许通配绑定与特定地址绑定共存；仅探测
/// 回环地址会把被 0.0.0.0 监听器占用的端口误判为空闲。
/// 现状：本项目所有模块 adapter 均经 `EP_HOST` 注入绑定 `127.0.0.1` 回环
/// （避免 Windows 防火墙弹窗），但残留进程或外部程序仍可能以 `0.0.0.0` 占用
/// 端口，该场景不可漏判。
/// 反向场景（`127.0.0.1:port` 被占）下 bind `0.0.0.0:port` 在双平台都会
/// 正确失败，由先执行的通配探测覆盖。
///
/// 注意：通配探测监听器必须先释放再探测回环——Linux 未设 SO_REUSEADDR 时
/// 通配绑定排斥特定地址的同端口绑定，持有它会导致后一探测误判占用。
///
/// 仍存在的盲区：只覆盖 IPv4 面；若端口仅被 IPv6（[::] / [::1]）监听器
/// 占用，本探测仍判空闲。TOCTOU 与 [`PortManager::allocate`] 注释相同：
/// 探测监听器释放后、模块进程实际绑定前，端口仍可能被抢占。
fn os_port_free(port: u16) -> bool {
    // 先探通配地址：0.0.0.0 监听器存在时 Windows 上回环探测仍可绑定成功，
    // 只有通配探测能捕获该占用形态
    let Ok(listener) = std::net::TcpListener::bind(("0.0.0.0", port)) else {
        return false;
    };
    // 立即释放再探回环（Linux 通配/特定地址绑定互斥，见函数注释）
    drop(listener);
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Mutex, MutexGuard};

    /// 端口测试需要真实 bind OS 临时端口做探测/预约。并行测试的 bind(:0)
    /// 会争抢其他测试刚释放的端口（分配器立即把刚空闲端口作为候选回收），
    /// 破坏「释放后应空闲」「allocate 必命中辅助函数预约的窗口/端口对」等
    /// 前提——本机实测并行争抢导致间歇性失败。此处串行化（风格同
    /// execution::TEST_LOCK）；本模块测试均为毫秒级，串行代价可忽略。
    static PORT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_port_tests() -> MutexGuard<'static, ()> {
        // 抗中毒：前序测试 panic 后后续测试仍可运行
        PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 测试辅助：通过 bind :0 找到 `size` 个连续的、当前 OS 层面空闲的
    /// 端口并返回窗口起始端口（预约监听器持有到返回前最后一刻才释放，
    /// 把 TOCTOU 窗口压到最小）。不硬编码端口号，避免被外部进程
    /// （如残留的孤儿模块进程、并行测试）占用导致测试不稳定。
    ///
    /// 锚点必须绑 `0.0.0.0:0` 而非 `127.0.0.1:0`：Windows 绑定共存语义下，
    /// 回环临时端口可能取到已被并行测试 0.0.0.0 监听器占用的端口
    /// （127.0.0.1 绑定可与其共存），通配锚点则必然取到双地址全空闲的端口。
    /// 预约用通配监听器持有期间，并行测试的 [`os_port_free`] 探测会判该
    /// 端口占用而避开，等同独占预约。
    fn find_free_window(size: u16) -> u16 {
        for _ in 0..32 {
            let first = TcpListener::bind(("0.0.0.0", 0)).expect("bind ephemeral port");
            let start = first.local_addr().expect("local_addr").port();
            if start > u16::MAX - size {
                continue;
            }
            let mut probes = vec![first];
            let mut ok = true;
            for offset in 1..size {
                match TcpListener::bind(("0.0.0.0", start + offset)) {
                    Ok(l) => probes.push(l),
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                drop(probes); // 最后一刻释放预约，窗口回到 OS 空闲状态
                return start;
            }
        }
        panic!("failed to find a window of {size} consecutive free ports after 32 attempts");
    }

    /// 测试辅助：找一对相邻端口（127.0.0.1 占用, 空闲）：第一个由返回的
    /// 回环监听器持续占住；第二个用通配监听器预约到返回前最后一刻
    /// （持有期间并行测试的探测判其占用而避开）。
    fn occupied_then_free_pair() -> (TcpListener, u16) {
        for _ in 0..32 {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
            let port = listener.local_addr().expect("local_addr").port();
            if port < u16::MAX {
                // 条件里的预约监听器在语句结束时（返回前最后一刻）释放
                if TcpListener::bind(("0.0.0.0", port + 1)).is_ok() {
                    return (listener, port);
                }
            }
        }
        panic!("failed to find adjacent (occupied, free) port pair after 32 attempts");
    }

    /// 测试辅助：找一对相邻端口（0.0.0.0 通配占用, 空闲）：第一个由返回的
    /// 通配监听器持续占住（与模块 adapter 的 bind 形态一致）；第二个用
    /// 通配监听器预约到返回前最后一刻（持有期间并行测试避开该端口）。
    fn wildcard_occupied_then_free_pair() -> (TcpListener, u16) {
        for _ in 0..32 {
            let listener =
                TcpListener::bind(("0.0.0.0", 0)).expect("bind 0.0.0.0 ephemeral port");
            let port = listener.local_addr().expect("local_addr").port();
            if port < u16::MAX {
                // 条件里的预约监听器在语句结束时（返回前最后一刻）释放
                if TcpListener::bind(("0.0.0.0", port + 1)).is_ok() {
                    return (listener, port);
                }
            }
        }
        panic!("failed to find adjacent (0.0.0.0-occupied, free) port pair after 32 attempts");
    }

    #[test]
    fn test_allocate_sequential() {
        let _guard = lock_port_tests();
        let start = find_free_window(3);
        let mut pm = PortManager::new(start, start + 10);
        assert_eq!(pm.allocate("mod-a").unwrap(), start);
        assert_eq!(pm.allocate("mod-b").unwrap(), start + 1);
        assert_eq!(pm.allocate("mod-c").unwrap(), start + 2);
        assert_eq!(pm.allocated_count(), 3);
    }

    #[test]
    fn test_allocate_idempotent() {
        let _guard = lock_port_tests();
        let start = find_free_window(1);
        let mut pm = PortManager::new(start, start + 10);
        let p1 = pm.allocate("mod-a").unwrap();
        let p2 = pm.allocate("mod-a").unwrap();
        assert_eq!(p1, p2);
        assert_eq!(pm.allocated_count(), 1);
    }

    #[test]
    fn test_release_and_reuse() {
        let _guard = lock_port_tests();
        // find_free_window 释放预约后、allocate 前存在 TOCTOU 窗口：并行
        // 测试（不持 PORT_TEST_LOCK）可能 bind 走窗口端口，allocate 的
        // OS 探测会跳过被占端口而落到窗口外。窗口被抢占时重试，最多 3 次。
        for _attempt in 0..3 {
            let start = find_free_window(1);
            let mut pm = PortManager::new(start, start + 2);
            let port = match pm.allocate("mod-a") {
                Ok(p) if p == start => p,
                _ => continue, // 窗口被并行测试抢占，重试
            };

            pm.release("mod-a");
            if pm.allocated_count() != 0 || !pm.is_available(start) {
                continue;
            }

            // 释放后端口可被重新分配
            if let Ok(start) = pm.allocate("mod-b") {
                assert_eq!(port, start);
                return;
            }
        }
        panic!("port window kept being preempted by parallel tests after 3 attempts");
    }

    #[test]
    fn test_range_exhausted() {
        let _guard = lock_port_tests();
        // 同上：窗口被并行测试抢占时 allocate 会落到窗口外，耗尽语义不成立，重试。
        for _attempt in 0..3 {
            let start = find_free_window(2);
            let mut pm = PortManager::new(start, start + 1);
            if pm.allocate("mod-a").is_err() || pm.allocate("mod-b").is_err() {
                continue;
            }

            if let Err(e) = pm.allocate("mod-c") {
                assert!(e.to_string().contains("exhausted"));
                return;
            }
        }
        panic!("port window kept being preempted by parallel tests after 3 attempts");
    }

    #[test]
    fn test_is_available() {
        let _guard = lock_port_tests();
        // 窗口须覆盖 start+5：is_available 现在含 OS 探测，候选须确认 OS 空闲。
        // 并行测试抢占窗口端口时探测失败，重试（见 test_release_and_reuse 注释）。
        for _attempt in 0..3 {
            let start = find_free_window(6);
            let mut pm = PortManager::new(start, start + 5);
            if !pm.is_available(start) || !pm.is_available(start + 5) {
                continue;
            }
            // 范围外
            assert!(!pm.is_available(start - 1));
            assert!(!pm.is_available(start + 6));

            // 已分配端口不再可用（用实际分配到的端口断言，避免窗口被抢时误判）
            let got = pm.allocate("mod-a").unwrap();
            assert!(!pm.is_available(got));
            assert!(pm.is_available(start + 1));
            return;
        }
        panic!("port window kept being preempted by parallel tests after 3 attempts");
    }

    #[test]
    fn test_get_port() {
        let _guard = lock_port_tests();
        let start = find_free_window(1);
        let mut pm = PortManager::new(start, start + 10);
        assert_eq!(pm.get_port("nonexistent"), None);

        pm.allocate("mod-a").unwrap();
        assert_eq!(pm.get_port("mod-a"), Some(start));

        pm.release("mod-a");
        assert_eq!(pm.get_port("mod-a"), None);
    }

    #[test]
    fn test_os_port_free_detects_occupancy() {
        let _guard = lock_port_tests();
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
        let port = listener.local_addr().expect("local_addr").port();

        // 端口被 listener 占住时，探测应判定为已占用
        assert!(!os_port_free(port));

        drop(listener);
        // 释放后应判定为空闲
        assert!(os_port_free(port));
    }

    #[test]
    fn test_os_port_free_detects_wildcard_occupancy() {
        let _guard = lock_port_tests();
        // 占用方 bind 0.0.0.0（与模块 adapter uvicorn host="0.0.0.0" 同形）。
        // Windows 上此时单独探测 127.0.0.1 仍可绑定成功（共存语义）会误判
        // 空闲——双地址探测修复的核心回归场景。
        let listener = TcpListener::bind(("0.0.0.0", 0)).expect("bind 0.0.0.0 ephemeral port");
        let port = listener.local_addr().expect("local_addr").port();

        assert!(!os_port_free(port));

        drop(listener);
        // 释放后应判定为空闲
        assert!(os_port_free(port));
    }

    #[test]
    fn test_allocate_fails_when_only_candidate_os_occupied() {
        let _guard = lock_port_tests();
        // 用临时监听器占住一个真实端口（bind :0，不硬编码端口号）
        let _listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
        let port = _listener.local_addr().expect("local_addr").port();

        // 范围内唯一候选被 OS 实际占用 → 跳过且范围耗尽，返回明确错误
        let mut pm = PortManager::new(port, port);
        let result = pm.allocate("mod-a");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("exhausted"));
        assert_eq!(pm.allocated_count(), 0);
    }

    #[test]
    fn test_allocate_skips_os_occupied_and_takes_next() {
        let _guard = lock_port_tests();
        let (_listener, occupied) = occupied_then_free_pair();
        let mut pm = PortManager::new(occupied, occupied + 1);

        // occupied 被外部监听器占住，allocate 应跳过它并分配下一个空闲端口
        assert_eq!(pm.allocate("mod-a").unwrap(), occupied + 1);
        assert_eq!(pm.get_port("mod-a"), Some(occupied + 1));
        assert_eq!(pm.allocated_count(), 1);
    }

    #[test]
    fn test_allocate_skips_wildcard_occupied_and_takes_next() {
        let _guard = lock_port_tests();
        // 0.0.0.0 被占（模块 adapter 的真实 bind 形态；Windows 上仅探回环
        // 会误判该端口空闲）→ allocate 必须跳过被占端口取下一个
        let (_listener, occupied) = wildcard_occupied_then_free_pair();
        let mut pm = PortManager::new(occupied, occupied + 1);

        assert_eq!(pm.allocate("mod-a").unwrap(), occupied + 1);
        assert_eq!(pm.get_port("mod-a"), Some(occupied + 1));
        assert_eq!(pm.allocated_count(), 1);
    }

    // ─── 复用语义回归：复用路径必须直接返回已持有端口 ──────────────────────
    // 模块运行中其端口被监听属正常状态——复用路径若做 OS 探测会把正常运行的
    // 模块误判为"占用"而错误换端口（孤儿进程端口冲突由 process.rs teardown
    // 的进程树回收兜底，不在此处探测）。

    #[test]
    fn test_allocate_reuse_returns_held_port_even_if_os_occupied() {
        let _guard = lock_port_tests();
        let start = find_free_window(2);
        let mut pm = PortManager::new(start, start + 1);

        // 首次分配：start 空闲
        assert_eq!(pm.allocate("mod-a").unwrap(), start);

        // OS 层占住 mod-a 已持有的端口（模拟运行中的模块服务监听）
        let _listener = TcpListener::bind(("127.0.0.1", start)).unwrap();

        // 复用路径必须直接返回已持有端口，不得因 OS 监听而换端口
        assert_eq!(pm.allocate("mod-a").unwrap(), start);
        assert_eq!(pm.get_port("mod-a"), Some(start));
        assert_eq!(pm.allocated_count(), 1);
    }

    // ─── P3 回归：is_available 须查 OS 层（与 allocate 语义对齐） ───────────

    #[test]
    fn test_is_available_checks_os_occupancy() {
        let _guard = lock_port_tests();
        // occupied 被回环监听器持续占住（进程内未分配）→ 应判不可用。
        // 旧实现只看进程内分配表，会误判空闲（P3 修复核心回归）。
        // 正向用例（空闲端口判可用）由 test_is_available 覆盖——空闲端口的
        // 二次探测与并行测试存在 TOCTOU 窗口，不在本测试中重复。
        let (listener, occupied) = occupied_then_free_pair();
        let pm = PortManager::new(occupied, occupied + 1);
        assert!(!pm.is_available(occupied));
        drop(listener);
    }
}
