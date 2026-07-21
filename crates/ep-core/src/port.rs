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
    /// 未被占用的端口。范围耗尽时返回错误。
    pub fn allocate(&mut self, module_id: &str) -> Result<u16> {
        // 已分配则直接返回
        if let Some(&port) = self.allocations.get(module_id) {
            debug!(module_id, port, "port already allocated, returning existing");
            return Ok(port);
        }

        // 收集已占用端口集合
        let used: std::collections::HashSet<u16> = self.allocations.values().copied().collect();

        for port in self.range_start..=self.range_end {
            if !used.contains(&port) {
                self.allocations.insert(module_id.to_string(), port);
                debug!(module_id, port, "allocated port");
                return Ok(port);
            }
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

    /// 检查端口是否在管理范围内且未被分配
    pub fn is_available(&self, port: u16) -> bool {
        if port < self.range_start || port > self.range_end {
            return false;
        }
        !self.allocations.values().any(|&p| p == port)
    }

    /// 当前已分配的端口数量
    pub fn allocated_count(&self) -> usize {
        self.allocations.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_sequential() {
        let mut pm = PortManager::new(18000, 18010);
        assert_eq!(pm.allocate("mod-a").unwrap(), 18000);
        assert_eq!(pm.allocate("mod-b").unwrap(), 18001);
        assert_eq!(pm.allocate("mod-c").unwrap(), 18002);
        assert_eq!(pm.allocated_count(), 3);
    }

    #[test]
    fn test_allocate_idempotent() {
        let mut pm = PortManager::new(18000, 18010);
        let p1 = pm.allocate("mod-a").unwrap();
        let p2 = pm.allocate("mod-a").unwrap();
        assert_eq!(p1, p2);
        assert_eq!(pm.allocated_count(), 1);
    }

    #[test]
    fn test_release_and_reuse() {
        let mut pm = PortManager::new(18000, 18002);
        let port = pm.allocate("mod-a").unwrap();
        assert_eq!(port, 18000);

        pm.release("mod-a");
        assert_eq!(pm.allocated_count(), 0);
        assert!(pm.is_available(18000));

        // 释放后端口可被重新分配
        let port2 = pm.allocate("mod-b").unwrap();
        assert_eq!(port2, 18000);
    }

    #[test]
    fn test_range_exhausted() {
        let mut pm = PortManager::new(18000, 18001);
        pm.allocate("mod-a").unwrap();
        pm.allocate("mod-b").unwrap();

        let result = pm.allocate("mod-c");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("exhausted"));
    }

    #[test]
    fn test_is_available() {
        let mut pm = PortManager::new(18000, 18005);
        assert!(pm.is_available(18000));
        assert!(pm.is_available(18005));
        // 范围外
        assert!(!pm.is_available(17999));
        assert!(!pm.is_available(18006));

        pm.allocate("mod-a").unwrap();
        assert!(!pm.is_available(18000));
        assert!(pm.is_available(18001));
    }

    #[test]
    fn test_get_port() {
        let mut pm = PortManager::new(18000, 18010);
        assert_eq!(pm.get_port("nonexistent"), None);

        pm.allocate("mod-a").unwrap();
        assert_eq!(pm.get_port("mod-a"), Some(18000));

        pm.release("mod-a");
        assert_eq!(pm.get_port("mod-a"), None);
    }
}
