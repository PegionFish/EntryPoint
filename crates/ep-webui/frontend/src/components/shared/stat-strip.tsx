import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

/** 单个统计项：大号等宽数字 + 渐变下划线 + 全大写灰阶标签（方案 §3.2 / 主张 4） */
export interface StatItem {
  /** 全大写灰阶小标签（中文不受 uppercase 影响，规则仅作用于拉丁字符） */
  label: string
  /** 数值（加载中可传 '–'），一律 mono + tabular 呈现 */
  value: ReactNode
  /** 数值着色类（如运行态青 / 异常红），缺省为前景色 */
  tone?: string
}

/**
 * 统计条带 StatStrip（§6.2-D / §9）：与桌面端 IA 对齐的页顶概览条。
 * 玻璃拟态容器；数字 text-4xl mono bold + 2px 品牌渐变下划线
 * （§3.1 规则 1 允许的 6 处渐变之一）。
 */
export function StatStrip({
  items,
  className,
}: {
  items: StatItem[]
  className?: string
}) {
  return (
    <div
      data-slot="stat-strip"
      className={cn(
        'glass-card grid grid-cols-2 gap-y-6 rounded-lg px-2 py-5 md:grid-cols-4',
        className,
      )}
    >
      {items.map((item) => (
        <div
          key={item.label}
          className="flex flex-col items-center justify-center px-3"
        >
          <div
            className={cn(
              'font-mono text-4xl leading-none font-bold tracking-tight tabular-nums',
              item.tone ?? 'text-foreground',
            )}
          >
            {item.value}
          </div>
          <div className="bg-gradient-accent mt-2 h-0.5 w-10 rounded-full" />
          <div className="mt-2.5 text-[11px] font-medium tracking-[0.08em] text-muted-foreground uppercase">
            {item.label}
          </div>
        </div>
      ))}
    </div>
  )
}
