import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'

/**
 * 加载骨架屏（见 DESIGN_SYSTEM §4.6：加载中用 Skeleton 占位，避免布局抖动）。
 */

export interface CardSkeletonProps {
  className?: string
}

/** 卡片骨架：标题行 + 操作按钮 + 三行正文 */
export function CardSkeleton({ className }: CardSkeletonProps) {
  return (
    <div className={cn('rounded-lg border bg-card p-6', className)}>
      <div className="flex items-center justify-between gap-4">
        <Skeleton className="h-5 w-32" />
        <Skeleton className="h-8 w-20 rounded-md" />
      </div>
      <div className="mt-5 space-y-2.5">
        <Skeleton className="h-4 w-full" />
        <Skeleton className="h-4 w-4/5" />
        <Skeleton className="h-4 w-3/5" />
      </div>
    </div>
  )
}

export interface TableSkeletonProps {
  /** 数据行数，默认 5 */
  rows?: number
  /** 列数，默认 4 */
  columns?: number
  className?: string
}

// 单元格宽度按 (行 + 列) 轮换，让骨架看起来更自然
const CELL_WIDTHS = ['w-2/5', 'w-1/5', 'w-1/4', 'w-1/3', 'w-1/6']

/** 表格骨架：表头 + N 行单元格 */
export function TableSkeleton({ rows = 5, columns = 4, className }: TableSkeletonProps) {
  return (
    <div className={cn('overflow-hidden rounded-lg border bg-card', className)}>
      <div className="flex items-center gap-4 border-b px-4 py-3">
        {Array.from({ length: columns }, (_, j) => (
          <Skeleton key={j} className="h-3.5 flex-1" />
        ))}
      </div>
      {Array.from({ length: rows }, (_, i) => (
        <div
          key={i}
          className="flex items-center gap-4 border-b px-4 py-3.5 last:border-b-0"
        >
          {Array.from({ length: columns }, (_, j) => (
            <div key={j} className="flex-1">
              <Skeleton className={cn('h-4', CELL_WIDTHS[(i + j) % CELL_WIDTHS.length])} />
            </div>
          ))}
        </div>
      ))}
    </div>
  )
}

export interface ListSkeletonProps {
  /** 列表项数，默认 4 */
  items?: number
  className?: string
}

/** 列表骨架：图标块 + 双行文本 + 右侧状态徽章 */
export function ListSkeleton({ items = 4, className }: ListSkeletonProps) {
  return (
    <div className={cn('space-y-3', className)}>
      {Array.from({ length: items }, (_, i) => (
        <div
          key={i}
          className="flex items-center gap-3 rounded-lg border bg-card p-3"
        >
          <Skeleton className="size-9 shrink-0 rounded-md" />
          <div className="min-w-0 flex-1 space-y-2">
            <Skeleton className="h-3.5 w-1/3" />
            <Skeleton className="h-3 w-2/3" />
          </div>
          <Skeleton className="h-6 w-16 shrink-0 rounded-full" />
        </div>
      ))}
    </div>
  )
}
