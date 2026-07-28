import { useNavigate } from 'react-router-dom'
import { ChevronRight } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { categoryLabel, statusMeta } from '@/lib/constants'
import { cn } from '@/lib/utils'
import type { ModuleResponse } from '@/api/types'

interface ModuleCardProps {
  module: ModuleResponse
  /** 实时运行状态（优先使用状态轮询结果，缺省时回退 service_status） */
  status?: string
  /** 服务端口（来自 /api/modules/:id/status，未运行时为 null） */
  port?: number | null
}

/** 模块卡片：状态点 + 名称 + 版本 + 类别徽章 + 端口，点击跳转详情页 */
export function ModuleCard({ module, status, port }: ModuleCardProps) {
  const navigate = useNavigate()
  const raw = status ?? module.service_status ?? ''
  const meta = statusMeta(raw)
  const key = raw
    .trim()
    .toLowerCase()
    .replace(/\s+/g, '_')
    .replace('notready', 'not_ready')
  // 运行中或过渡态（starting / preparing）时状态点呼吸闪烁
  const alive = key === 'running' || meta.transitional

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={() => navigate(`/modules/${module.id}`)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          navigate(`/modules/${module.id}`)
        }
      }}
      className={cn(
        'group cursor-pointer rounded-lg border border-border bg-card p-4 text-left',
        'transition-all duration-200 hover:-translate-y-0.5 hover:border-primary/40 hover:shadow-lg hover:shadow-primary/5',
        'focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50',
      )}
    >
      <div className="flex items-start justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <span
            className={cn(
              'size-2 shrink-0 rounded-full',
              meta.dot,
              alive && 'animate-pulse',
            )}
            aria-hidden
          />
          <span className="truncate text-sm font-medium">{module.name}</span>
        </div>
        <ChevronRight
          className="size-4 shrink-0 text-muted-foreground/40 transition-all duration-200 group-hover:translate-x-0.5 group-hover:text-primary"
          aria-hidden
        />
      </div>

      <div className="mt-3 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <Badge variant="secondary" className="shrink-0 text-[11px]">
            {categoryLabel(module.category)}
          </Badge>
          {module.version && (
            <span className="truncate font-mono text-xs text-muted-foreground">
              v{module.version}
            </span>
          )}
        </div>
        <span
          className={cn(
            'shrink-0 font-mono text-xs tabular-nums',
            port != null ? 'text-foreground/80' : 'text-muted-foreground/50',
          )}
        >
          {port != null ? `:${port}` : '—'}
        </span>
      </div>
    </div>
  )
}
