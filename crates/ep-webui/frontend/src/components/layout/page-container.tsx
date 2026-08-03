import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

interface PageContainerProps {
  /** 页面标题 */
  title: string
  /** 页面描述（可选） */
  description?: string
  /** 右上角操作区（可选） */
  actions?: ReactNode
  children: ReactNode
  className?: string
}

/** 页面通用容器：标题栏 + 可滚动内容区 */
export function PageContainer({
  title,
  description,
  actions,
  children,
  className,
}: PageContainerProps) {
  return (
    <div className={cn('flex h-full flex-col overflow-hidden', className)}>
      <div className="flex shrink-0 items-start justify-between gap-4 border-b border-border px-4 py-3 sm:px-6 sm:py-4">
        <div className="min-w-0">
          <h1 className="truncate text-xl font-semibold tracking-tight">
            {title}
          </h1>
          {description && (
            <p className="mt-1 text-sm text-muted-foreground">{description}</p>
          )}
        </div>
        {actions && (
          <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">
            {actions}
          </div>
        )}
      </div>
      <div className="flex-1 overflow-y-auto p-4 sm:p-6">{children}</div>
    </div>
  )
}
