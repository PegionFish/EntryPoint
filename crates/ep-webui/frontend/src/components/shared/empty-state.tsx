import type { LucideIcon } from 'lucide-react'
import { Database, GitBranch, ListTodo, Puzzle } from 'lucide-react'
import { useTranslation } from 'react-i18next'

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
  const { t } = useTranslation('components')
  return (
    <EmptyState
      icon={Puzzle}
      title={t('emptyState.noModules.title')}
      description={description ?? t('emptyState.noModules.description')}
      action={action}
      className={className}
    />
  )
}

/** 任务队列为空 */
export function NoTasksState({ description, action, className }: EmptyStatePresetProps) {
  const { t } = useTranslation('components')
  return (
    <EmptyState
      icon={ListTodo}
      title={t('emptyState.noTasks.title')}
      description={description ?? t('emptyState.noTasks.description')}
      action={action}
      className={className}
    />
  )
}

/** 模型列表为空 */
export function NoModelsState({ description, action, className }: EmptyStatePresetProps) {
  const { t } = useTranslation('components')
  return (
    <EmptyState
      icon={Database}
      title={t('emptyState.noModels.title')}
      description={description ?? t('emptyState.noModels.description')}
      action={action}
      className={className}
    />
  )
}

/** 管线列表为空 */
export function NoPipelinesState({ description, action, className }: EmptyStatePresetProps) {
  const { t } = useTranslation('components')
  return (
    <EmptyState
      icon={GitBranch}
      title={t('emptyState.noPipelines.title')}
      description={description ?? t('emptyState.noPipelines.description')}
      action={action}
      className={className}
    />
  )
}
