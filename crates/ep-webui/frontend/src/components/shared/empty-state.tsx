import type { LucideIcon } from 'lucide-react'
import { Database, GitBranch, ListTodo, Puzzle } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

export interface EmptyStateAction {
  label: string
  onClick: () => void
}

export interface EmptyStateProps {
  icon?: LucideIcon
  title: string
  description?: string
  action?: EmptyStateAction
  className?: string
}

/**
 * 空状态：居中图标 + 标题 + 说明 + 可选主操作按钮。
 * 见 DESIGN_SYSTEM §4.6（空数据：居中图标 + 辅助文本说明，必要时提供主操作按钮）。
 */
export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  className,
}: EmptyStateProps) {
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center gap-3 px-6 py-14 text-center',
        className,
      )}
    >
      {Icon && (
        <div className="flex size-14 items-center justify-center rounded-lg border border-dashed border-border bg-muted/30 text-muted-foreground transition-colors duration-300">
          <Icon className="size-7 opacity-70" strokeWidth={1.5} />
        </div>
      )}
      <div className="space-y-1">
        <p className="text-sm font-semibold text-foreground">{title}</p>
        {description && (
          <p className="mx-auto max-w-sm text-xs leading-relaxed text-muted-foreground">
            {description}
          </p>
        )}
      </div>
      {action && (
        <Button variant="outline" size="sm" className="mt-1" onClick={action.onClick}>
          {action.label}
        </Button>
      )}
    </div>
  )
}

export interface EmptyStatePresetProps {
  /** 覆盖默认说明文案 */
  description?: string
  action?: EmptyStateAction
  className?: string
}

/** 模块列表为空 */
export function NoModulesState({ description, action, className }: EmptyStatePresetProps) {
  return (
    <EmptyState
      icon={Puzzle}
      title="暂无模块"
      description={description ?? '尚未发现可用模块，请检查模块目录配置后刷新'}
      action={action}
      className={className}
    />
  )
}

/** 任务队列为空 */
export function NoTasksState({ description, action, className }: EmptyStatePresetProps) {
  return (
    <EmptyState
      icon={ListTodo}
      title="暂无任务"
      description={description ?? '任务队列为空，提交新任务后将在此显示执行进度'}
      action={action}
      className={className}
    />
  )
}

/** 模型列表为空 */
export function NoModelsState({ description, action, className }: EmptyStatePresetProps) {
  return (
    <EmptyState
      icon={Database}
      title="暂无模型"
      description={description ?? '尚未下载或导入模型，前往模型页面添加所需模型'}
      action={action}
      className={className}
    />
  )
}

/** 管线列表为空 */
export function NoPipelinesState({ description, action, className }: EmptyStatePresetProps) {
  return (
    <EmptyState
      icon={GitBranch}
      title="暂无管线"
      description={description ?? '尚未创建处理管线，尝试编排你的第一条流水线'}
      action={action}
      className={className}
    />
  )
}
