import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

/** 单个分段选项 */
export interface SegmentedTabItem<T extends string = string> {
  /** 选项值（受控状态的真源） */
  value: T
  /** 展示标签 */
  label: ReactNode
  /** 计数徽标（缺省不展示）；mono 数字呈现 */
  count?: number
  /** 激活态点缀色类（如 text-status-error），缺省跟随主色 */
  tone?: string
}

/**
 * 分段筛选器 SegmentedTabs（方案 §9 / §7.4）：任务状态筛选等场景的
 * 玻璃拟态分段控件。激活项 = primary/10 底 + 发光描边；hover 仅提升
 * 描边亮度（零位移，§1 主张 3）。
 */
export function SegmentedTabs<T extends string = string>({
  items,
  value,
  onChange,
  className,
  ariaLabel,
}: {
  items: SegmentedTabItem<T>[]
  value: T
  onChange: (value: T) => void
  className?: string
  ariaLabel?: string
}) {
  return (
    <div
      data-slot="segmented-tabs"
      role="tablist"
      aria-label={ariaLabel}
      className={cn(
        'glass-card inline-flex max-w-full flex-wrap items-center gap-1 rounded-lg p-1',
        className,
      )}
    >
      {items.map((item) => {
        const active = item.value === value
        return (
          <button
            key={item.value}
            type="button"
            role="tab"
            aria-selected={active}
            onClick={() => onChange(item.value)}
            className={cn(
              'inline-flex cursor-pointer items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
              'duration-(--duration-fast) ease-(--ease-standard)',
              active
                ? cn(
                    'glow-primary bg-primary/10 text-foreground',
                    item.tone ?? 'text-foreground',
                  )
                : 'text-muted-foreground hover:bg-muted/60 hover:text-foreground',
            )}
          >
            {item.label}
            {item.count !== undefined && (
              <span
                className={cn(
                  'rounded-full px-1.5 py-px font-mono text-[10px] tabular-nums',
                  active
                    ? 'bg-primary/15 text-primary'
                    : 'bg-muted text-muted-foreground',
                )}
              >
                {item.count}
              </span>
            )}
          </button>
        )
      })}
    </div>
  )
}
