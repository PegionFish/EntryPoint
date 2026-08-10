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
