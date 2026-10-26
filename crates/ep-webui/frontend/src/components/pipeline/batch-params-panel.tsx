import { useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { X } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
import { ParamSpecField } from '@/components/shared/pipeline-node'
import type { ParamValue, PipelineFlowNode } from '@/components/shared/pipeline-node'
import { cn } from '@/lib/utils'

import { commonParamShape, paramFieldStates } from './selection-utils'

/**
 * T-28b 批量参数编辑面板（多选同类型节点时浮出）：
 * 多选全部为同一 builtin、或同模块同能力时，展示其公共参数表；
 * 改一个值应用到全部选中节点（页面层负责提交 undo 命令）。
 * 取值不一致的字段显示「混合」标记，回填值为首个选中节点的当前值。
 */
export function BatchParamsPanel({
  nodes,
  selectedIds,
  onApply,
  onClose,
  className,
}: {
  /** 画布全部节点（内部按 selectedIds 过滤） */
  nodes: PipelineFlowNode[]
  /** 当前多选节点 id（≥2 才渲染） */
  selectedIds: string[]
  /** 应用一个参数值到全部选中节点 */
  onApply: (name: string, value: ParamValue) => void
  onClose: () => void
  /** 附加布局类（窄屏 overlay 定位等） */
  className?: string
}) {
  const { t } = useTranslation('pipeline')
  const shape = useMemo(() => commonParamShape(nodes, selectedIds), [nodes, selectedIds])
  const fieldStates = useMemo(
    () => (shape ? paramFieldStates(nodes, selectedIds, shape.specs) : []),
    [nodes, selectedIds, shape],
  )

  if (selectedIds.length < 2 || !shape) return null

  return (
    <aside
      className={cn(
        'glass flex h-full w-72 shrink-0 flex-col border-l border-border-glow',
        className,
      )}
      data-testid="batch-params-panel"
    >
      <div className="flex shrink-0 items-start justify-between gap-2 border-b border-border px-4 py-3">
        <div className="min-w-0">
          <p className="text-sm font-semibold">
            {t('batchParams.title', { defaultValue: '批量参数' })}
          </p>
          <p className="mt-0.5 truncate text-xs text-muted-foreground">
            {t('batchParams.subtitle', {
              defaultValue: '{{count}} 个同类型节点 · 修改即应用到全部',
              count: selectedIds.length,
            })}
          </p>
        </div>
        <Button
          variant="ghost"
          size="icon-xs"
          onClick={onClose}
          aria-label={t('batchParams.closeAria', { defaultValue: '关闭批量参数面板' })}
          title={t('batchParams.closeAria', { defaultValue: '关闭批量参数面板' })}
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>

      <ScrollArea className="flex-1">
        <div className="space-y-4 p-4">
          {fieldStates.length === 0 ? (
            <p className="text-xs text-muted-foreground">
              {t('batchParams.noParams', { defaultValue: '该类型节点没有可编辑参数' })}
            </p>
          ) : (
            fieldStates.map(({ spec, value, mixed }) => (
              <div key={spec.name} className="space-y-1.5">
                <div className="flex items-center gap-1.5">
                  <span className="block truncate whitespace-nowrap text-xs font-medium">
                    {spec.label}
                  </span>
                  {mixed && (
                    <span
                      className="shrink-0 rounded-full border border-status-starting/40 bg-status-starting/10 px-1.5 py-px text-[9px] text-status-starting"
                      title={t('batchParams.mixedHint', {
                        defaultValue: '选区内该参数取值不一致；保存后统一为下方值',
                      })}
                    >
                      {t('batchParams.mixed', { defaultValue: '混合' })}
                    </span>
                  )}
                </div>
                <ParamSpecField
                  spec={spec}
                  value={value}
                  onChange={(v) => onApply(spec.name, v)}
                />
              </div>
            ))
          )}
          <p className="rounded-md border border-border bg-muted/30 px-2.5 py-2 text-[10px] text-muted-foreground">
            {t('batchParams.hint', {
              defaultValue: '修改仅作用于选中的 {{count}} 个节点，可 Ctrl+Z 一步撤销',
              count: selectedIds.length,
            })}
          </p>
        </div>
      </ScrollArea>
    </aside>
  )
}
