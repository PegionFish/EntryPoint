import { Badge } from '@/components/ui/badge'
import { STATUS_COLORS, statusMeta } from '@/lib/constants'
import { cn } from '@/lib/utils'

export interface StatusBadgeProps {
  /** 后端状态值（running / stopped / starting / preparing / error / not_ready，未知值原样显示） */
  status: string
  /** 徽章尺寸，默认 md */
  size?: 'sm' | 'md'
  className?: string
}

/**
 * 状态徽章：彩色圆点 + 中文状态文本。
 *
 * - 配色与中文标签取自 `STATUS_COLORS`（见 DESIGN_SYSTEM §1.3 / §4.1）。
 * - 过渡态（starting / preparing）圆点附加脉冲动画；
 *   running 状态圆点同样脉冲并带微光，强调"正在运行"的实时感。
 */
export function StatusBadge({ status, size = 'md', className }: StatusBadgeProps) {
  const meta = statusMeta(status)
  // statusMeta 对已知状态返回 STATUS_COLORS 中的同一对象引用，可用引用比较判断 running
  const isRunning = meta === STATUS_COLORS.running
  const showPulse = isRunning || meta.transitional

  return (
    <Badge
      variant="outline"
      className={cn(
        'gap-1.5 font-normal',
        meta.badge,
        size === 'sm' ? 'px-1.5 py-0 text-[11px]' : 'px-2 py-0.5 text-xs',
        className,
      )}
    >
      <span
        aria-hidden
        className={cn(
          'shrink-0 rounded-full',
          size === 'sm' ? 'size-1.5' : 'size-2',
          meta.dot,
          showPulse && 'animate-pulse',
          isRunning && 'shadow-[0_0_6px_1px] shadow-status-running/40',
        )}
      />
      {meta.label}
    </Badge>
  )
}
