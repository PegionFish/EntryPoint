import { Link, useNavigate } from 'react-router-dom'
import { ChevronRight, LoaderCircle, Play, Square, TriangleAlert } from 'lucide-react'
import { useTranslation } from 'react-i18next'
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
  /** 提供时且模块已停止，在卡片右上角显示启动按钮 */
  onStart?: () => void
  /** 启动请求进行中：启动按钮显示旋转图标并禁用 */
  starting?: boolean
  /** 提供时且模块运行中，在卡片右上角显示停止按钮（回调内自行弹确认框） */
  onStop?: () => void
}

/** 模块卡片：状态点 + 名称 + 版本 + 类别徽章 + 端口，点击跳转详情页 */
export function ModuleCard({
  module,
  status,
  port,
  onStart,
  starting,
  onStop,
}: ModuleCardProps) {
  const { t } = useTranslation('components')
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
        <div className="flex shrink-0 items-center gap-0.5">
          {onStart && key === 'stopped' && (
            <button
              type="button"
              aria-label={t('moduleCard.start', { name: module.name })}
              title={t('moduleCard.start', { name: module.name })}
              disabled={starting}
              onClick={(e) => {
                e.stopPropagation()
                onStart()
              }}
              onKeyDown={(e) => {
                // 避免 Enter/Space 冒泡到卡片的导航逻辑
                e.stopPropagation()
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault()
                  if (!starting) onStart()
                }
              }}
              className="rounded-md p-1 text-muted-foreground/60 transition-colors hover:bg-status-running/10 hover:text-status-running focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50"
            >
              {starting ? (
                <LoaderCircle className="size-3.5 animate-spin" aria-hidden />
              ) : (
                <Play className="size-3.5" aria-hidden />
              )}
            </button>
          )}
          {onStop && (key === 'running' || meta.transitional) && (
            <button
              type="button"
              aria-label={t('moduleCard.stop', { name: module.name })}
              title={t('moduleCard.stop', { name: module.name })}
              onClick={(e) => {
                e.stopPropagation()
                onStop()
              }}
              onKeyDown={(e) => {
                // 避免 Enter/Space 冒泡到卡片的导航逻辑
                e.stopPropagation()
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault()
                  onStop()
                }
              }}
              className="rounded-md p-1 text-muted-foreground/60 transition-colors hover:bg-status-error/10 hover:text-status-error focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
            >
              <Square className="size-3.5" aria-hidden />
            </button>
          )}
          <ChevronRight
            className="size-4 shrink-0 text-muted-foreground/40 transition-all duration-200 group-hover:translate-x-0.5 group-hover:text-primary"
            aria-hidden
          />
        </div>
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

      {/* 未就绪引导：明确原因（缺模型/依赖）并给出处理入口，包裹层阻止事件冒泡到卡片导航 */}
      {key === 'not_ready' && (
        <div
          className="mt-3 flex items-center justify-between gap-2 rounded-md border border-status-preparing/30 bg-status-preparing/10 px-2.5 py-1.5"
          onClick={(e) => e.stopPropagation()}
          onKeyDown={(e) => e.stopPropagation()}
        >
          <span className="flex min-w-0 items-center gap-1.5 text-xs text-status-preparing">
            <TriangleAlert className="size-3.5 shrink-0" aria-hidden />
            {t('moduleCard.missingDeps')}
          </span>
          <span className="flex shrink-0 items-center gap-2.5 text-xs font-medium">
            <Link
              to="/models"
              className="rounded-sm text-status-preparing underline-offset-2 transition-colors hover:text-status-preparing/80 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
            >
              {t('moduleCard.manageModels')}
            </Link>
            <Link
              to="/"
              className="rounded-sm text-status-preparing underline-offset-2 transition-colors hover:text-status-preparing/80 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
            >
              {t('moduleCard.dependencyReport')}
            </Link>
          </span>
        </div>
      )}
    </div>
  )
}
