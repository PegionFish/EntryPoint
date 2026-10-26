import { useTranslation } from 'react-i18next'
import {
  AlignHorizontalDistributeCenter,
  AlignStartHorizontal,
  AlignStartVertical,
  Copy,
  Play,
  Trash2,
  X,
} from 'lucide-react'

import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'

import type { AlignMode } from './selection-utils'

/**
 * T-28b 批量操作工具条（多选 ≥2 时浮出于画布顶部中央）：
 * 删除 / 克隆 / 对齐·均布 / 作为组运行（T-28c，≥2 个可直跑模块节点时可用）。
 * 纯展示组件：全部动作经 props 上抛，由页面层提交 undo 命令。
 */
export function MultiSelectToolbar({
  count,
  canRunGroup,
  onDelete,
  onClone,
  onAlign,
  onRunGroup,
  onClear,
}: {
  /** 当前多选节点数 */
  count: number
  /** T-28c：选区内含 ≥2 个「模块 + 能力」节点时可用「作为组运行」 */
  canRunGroup: boolean
  onDelete: () => void
  onClone: () => void
  onAlign: (mode: AlignMode) => void
  onRunGroup: () => void
  onClear: () => void
}) {
  const { t } = useTranslation('pipeline')

  const alignItems: { mode: AlignMode; label: string; icon: typeof AlignStartHorizontal }[] = [
    {
      mode: 'left',
      label: t('multiSelect.alignLeft', { defaultValue: '左对齐' }),
      icon: AlignStartVertical,
    },
    {
      mode: 'top',
      label: t('multiSelect.alignTop', { defaultValue: '顶对齐' }),
      icon: AlignStartHorizontal,
    },
    {
      mode: 'hdist',
      label: t('multiSelect.hdist', { defaultValue: '水平均布' }),
      icon: AlignHorizontalDistributeCenter,
    },
    {
      mode: 'vdist',
      label: t('multiSelect.vdist', { defaultValue: '垂直均布' }),
      icon: AlignHorizontalDistributeCenter,
    },
  ]

  return (
    <div
      className="glass flex items-center gap-1 rounded-lg border border-border-glow px-1.5 py-1 shadow-md"
      data-testid="multi-select-toolbar"
    >
      <span className="shrink-0 rounded-md bg-primary/10 px-1.5 py-0.5 text-[11px] font-medium text-primary">
        {t('multiSelect.count', { defaultValue: '已选 {{count}}', count })}
      </span>

      <Button
        variant="ghost"
        size="icon-xs"
        onClick={onDelete}
        title={t('multiSelect.delete', { defaultValue: '删除选中节点' })}
        aria-label={t('multiSelect.delete', { defaultValue: '删除选中节点' })}
        className="text-destructive hover:bg-destructive/10 hover:text-destructive"
      >
        <Trash2 className="h-3.5 w-3.5" />
      </Button>

      <Button
        variant="ghost"
        size="icon-xs"
        onClick={onClone}
        title={t('multiSelect.clone', { defaultValue: '复制选中节点（含内部连线）' })}
        aria-label={t('multiSelect.clone', { defaultValue: '复制选中节点（含内部连线）' })}
      >
        <Copy className="h-3.5 w-3.5" />
      </Button>

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon-xs"
            title={t('multiSelect.alignTitle', { defaultValue: '对齐 / 均布' })}
            aria-label={t('multiSelect.alignTitle', { defaultValue: '对齐 / 均布' })}
          >
            <AlignHorizontalDistributeCenter className="h-3.5 w-3.5" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="center" className="min-w-32">
          {alignItems.map((item) => (
            <DropdownMenuItem key={item.mode} onSelect={() => onAlign(item.mode)}>
              <item.icon className="h-3.5 w-3.5" />
              {item.label}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>

      {canRunGroup && (
        <Button
          variant="ghost"
          size="xs"
          onClick={onRunGroup}
          title={t('runGroup.title', { defaultValue: '把选中节点的直跑能力批量提交为任务组' })}
          className="text-primary hover:bg-primary/10"
        >
          <Play className="h-3.5 w-3.5" />
          <span className="hidden xl:inline">
            {t('runGroup.button', { defaultValue: '作为组运行' })}
          </span>
        </Button>
      )}

      <Button
        variant="ghost"
        size="icon-xs"
        onClick={onClear}
        title={t('multiSelect.clear', { defaultValue: '取消选择（Esc）' })}
        aria-label={t('multiSelect.clear', { defaultValue: '取消选择（Esc）' })}
      >
        <X className="h-3.5 w-3.5" />
      </Button>
    </div>
  )
}
