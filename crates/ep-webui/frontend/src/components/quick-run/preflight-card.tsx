import { useTranslation } from 'react-i18next'
import { Loader2, TriangleAlert } from 'lucide-react'
import { cn } from '@/lib/utils'
import {
  formatMbCompact,
  hasOverPool,
  type PoolBalanceRow,
} from '@/components/quick-run/pool-balance'

/**
 * 提交前 pre-flight 卡（T-21）：每池一行进度条（已用 + 估算 / 容量），
 * 超支行黄红告警。纯展示；行集由 pool-balance.ts 的纯函数产出，
 * 超支判定与确认对话框由调用方（run-workbench / 管线编辑器 / 导入预览）驱动。
 */
export function PoolPreflightCard({
  rows,
  loading = false,
  error = null,
  title,
  highlightNode = null,
  className,
}: {
  rows: PoolBalanceRow[]
  loading?: boolean
  error?: string | null
  /** 卡片标题（调用方给 i18n 文案） */
  title: string
  /** 选中节点 id：条目命中时高亮该节点对各池的贡献（管线编辑器用） */
  highlightNode?: string | null
  className?: string
}) {
  const { t } = useTranslation('run')
  const anyOver = hasOverPool(rows)
  return (
    <div
      className={cn(
        'space-y-2 rounded-lg border px-3 py-2.5',
        anyOver
          ? 'border-status-starting/40 bg-status-starting/5'
          : 'border-border bg-muted/20',
        className,
      )}
    >
      <div className="flex items-center gap-1.5">
        <p className="text-xs font-medium">{title}</p>
        {loading && (
          <Loader2 className="size-3 animate-spin text-muted-foreground" />
        )}
        {anyOver && (
          <span className="ml-auto inline-flex items-center gap-1 text-[10px] font-medium text-status-starting">
            <TriangleAlert className="size-3" />
            {t('preflight.overBadge')}
          </span>
        )}
      </div>

      {error && <p className="text-[11px] text-muted-foreground">{error}</p>}

      {rows.map((row) => {
        const unknown = row.totalMb === null
        const usedPct =
          unknown || row.usedMb === null
            ? 0
            : Math.min(100, (row.usedMb / row.totalMb!) * 100)
        const estPct = unknown
          ? 0
          : Math.min(
              Math.max(100 - usedPct, 0),
              (row.committedMb / row.totalMb!) * 100,
            )
        return (
          <div key={`${row.kind}:${row.deviceId}`} className="space-y-1">
            <div className="flex items-baseline justify-between gap-2">
              <span className="min-w-0 truncate font-mono text-[11px] font-medium">
                {row.kind === 'vram'
                  ? `VRAM · ${row.deviceId}`
                  : row.kind === 'ram'
                    ? 'RAM'
                    : 'UMA'}
                <span className="ml-1 font-sans text-[10px] font-normal text-muted-foreground">
                  {row.deviceName}
                </span>
              </span>
              <span
                className={cn(
                  'shrink-0 font-mono text-[10px]',
                  row.over ? 'text-status-error' : 'text-muted-foreground',
                )}
              >
                {unknown
                  ? t('preflight.unknownCapacity')
                  : `${formatMbCompact(row.usedMb)} + ${formatMbCompact(row.committedMb)} / ${formatMbCompact(row.totalMb)}`}
              </span>
            </div>
            {unknown ? (
              <div className="h-1.5 rounded-full bg-muted" />
            ) : (
              <div className="flex h-1.5 overflow-hidden rounded-full bg-muted">
                <div
                  className="bg-muted-foreground/50"
                  style={{ width: `${usedPct}%` }}
                />
                <div
                  className={cn(row.over ? 'bg-status-error' : 'bg-primary')}
                  style={{ width: `${estPct}%` }}
                />
              </div>
            )}
            {highlightNode !== null && row.entries.length > 0 && (
              <ul className="space-y-0.5">
                {row.entries.map((item) => (
                  <li
                    key={item.node}
                    className={cn(
                      'flex items-baseline justify-between gap-2 text-[10px]',
                      item.node === highlightNode
                        ? 'font-medium text-primary'
                        : 'text-muted-foreground/70',
                    )}
                  >
                    <span className="min-w-0 truncate">{item.node}</span>
                    <span className="shrink-0 font-mono">
                      {formatMbCompact(item.estimate_mb)}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )
      })}
    </div>
  )
}
