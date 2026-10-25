import type { DeviceResponse, VramBudgetResponse, VramPoolKind } from '@/api/types'

/**
 * 多池「估算 vs 实时余额」纯计算（T-21）。
 *
 * 独立成无组件模块：供 quick-run pre-flight 卡（提交前）与管线编辑器
 * 账本面板 / 导入预览复用；与组件分离以满足 react-refresh 单组件导出约束。
 */

/** 池条目（节点贡献明细） */
export interface PoolBalanceEntry {
  node: string
  estimate_mb: number
}

/**
 * 提交前 pre-flight 的单池视图行（T-21）：
 * 多池估算（VramBudgetResponse.pools，T-20）× 实时余额（/api/devices）。
 */
export interface PoolBalanceRow {
  kind: VramPoolKind
  /** 池标识：vram = 设备 id（cuda:0…）；ram 固定 "cpu" */
  deviceId: string
  /** 设备展示名（/api/devices name；缺失回退 id） */
  deviceName: string
  /** 管线对该池的估算（MB；ram 池为求和语义） */
  committedMb: number
  /** 实时占用（MB；快照缺失为 null） */
  usedMb: number | null
  /** 池容量（MB；unknown=true 或后端缺 pools 时为 null = 未知） */
  totalMb: number | null
  /** 估算 + 已用 是否超出容量（容量未知恒 false，不虚报风险） */
  over: boolean
  /** 节点贡献明细（选中节点高亮用） */
  entries: PoolBalanceEntry[]
}

/** MB → 紧凑显示（与管线页 formatMb 同口径：≥1024 MB 转 GB） */
export function formatMbCompact(mb: number | null | undefined): string {
  if (mb === null || mb === undefined) return '—'
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`
  return `${mb} MB`
}

/**
 * 多池响应 → 「估算 vs 实时余额」行集（纯函数）。
 *
 * 实时占用 join：vram 池按 device_id 对 `/api/devices`（或报告内
 * devices[].used_mb，同源快照）；ram 池固定取 CPU 条目。容量未知
 * （pools[].unknown）记 totalMb=null 且不判超支。
 */
export function poolBalanceRows(
  report: VramBudgetResponse | null | undefined,
  devices?: DeviceResponse[] | null,
): PoolBalanceRow[] {
  const pools = report?.pools
  if (!pools || pools.length === 0) return []
  const usedOf = (deviceId: string): number | null => {
    const fromDevices = devices?.find((d) => d.id === deviceId)
    if (fromDevices) return fromDevices.used_memory_mb
    const fromReport = report?.devices.find((d) => d.device_id === deviceId)
    return fromReport ? fromReport.used_mb : null
  }
  const nameOf = (deviceId: string): string =>
    devices?.find((d) => d.id === deviceId)?.name ?? deviceId
  return pools.map((p) => {
    const totalMb = p.unknown ? null : p.total_mb
    const usedMb = usedOf(p.device_id)
    const over =
      totalMb !== null &&
      (usedMb ?? 0) + p.committed_mb > totalMb &&
      p.committed_mb > 0
    return {
      kind: p.kind,
      deviceId: p.device_id,
      deviceName: p.kind === 'ram' ? p.device_id : nameOf(p.device_id),
      committedMb: p.committed_mb,
      usedMb,
      totalMb,
      over,
      entries: p.entries.map((e) => ({
        node: e.node,
        estimate_mb: e.estimate_mb,
      })),
    }
  })
}

/** 是否存在超支池（容量未知不参与判定） */
export function hasOverPool(rows: PoolBalanceRow[]): boolean {
  return rows.some((r) => r.over)
}
