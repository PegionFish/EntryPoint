import { useTranslation } from 'react-i18next'
import type { CapabilityDecl } from '@/api/types'
import { categoryLabel } from '@/lib/constants'
import { cn } from '@/lib/utils'

/** 能力目录条目（模块级入口：一个模型一个入口，能力在功能页内选择） */
export interface CatalogEntry {
  /** 选择键 = moduleId（模块粒度） */
  key: string
  moduleId: string
  moduleName: string
  category: string
  /** 模块声明的全部能力（工作台内选择） */
  capabilities: CapabilityDecl[]
  /** ModuleResponse.service_status 原值（running/starting/preparing/stopped/error…） */
  serviceStatus: string
  /** 运行中实例当前绑定的设备名（如 "cuda:0"；未运行为 null） */
  device: string | null
  /** manifest 声明的计算后端（D-Device 兼容性过滤依据） */
  backends: string[]
  /**
   * 激活变体是否就绪（join GET /api/models 按 status==='ready'）；
   * null = 该模块不依赖模型权重
   */
  modelReady: boolean | null
}

/** 服务状态点配色：运行绿 / 过渡蓝脉冲 / 错误红 / 其余灰 */
function statusDotClass(status: string): string {
  const s = (status ?? '').trim().toLowerCase()
  if (s === 'running') return 'bg-status-running'
  if (s === 'starting' || s === 'preparing')
    return 'bg-status-starting animate-pulse'
  if (s === 'error') return 'bg-status-error'
  return 'bg-muted-foreground/60'
}

/**
 * 快速调用 · 模型入口目录左栏：
 * 一个模型一个入口（分类筛选后的模块列表），单击选中进入右侧功能页，
 * 具体能力在功能页内选择（QUICK_RUN_PLAN D1 修订：模型粒度入口）。
 */
export function CapabilityCatalog({
  entries,
  selectedKey,
  onSelect,
}: {
  entries: CatalogEntry[]
  selectedKey: string | null
  onSelect: (key: string) => void
}) {
  const { t } = useTranslation('run')

  if (entries.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-border p-6 text-center text-sm text-muted-foreground">
        {t('noCapabilities')}
      </div>
    )
  }

  return (
    <div className="space-y-1.5">
      {entries.map((e) => {
        const active = e.key === selectedKey
        const caps = e.capabilities
        return (
          <button
            key={e.key}
            type="button"
            onClick={() => onSelect(e.key)}
            className={cn(
              'w-full cursor-pointer rounded-lg border px-3 py-2.5 text-left transition-colors',
              active
                ? 'border-primary/40 bg-primary/10'
                : 'border-border bg-card hover:border-primary/30 hover:bg-accent/50',
            )}
          >
            <div className="flex items-center gap-2">
              {/* 服务状态点 */}
              <span
                aria-hidden
                className={cn(
                  'size-2 shrink-0 rounded-full',
                  statusDotClass(e.serviceStatus),
                )}
              />
              <span className="truncate text-sm font-medium">
                {e.moduleName}
              </span>
              <span className="ml-auto shrink-0 rounded-full border border-border bg-muted/60 px-1.5 py-px text-[10px] text-muted-foreground">
                {categoryLabel(e.category)}
              </span>
            </div>
            <div className="mt-1 flex items-center gap-2 pl-4">
              <span className="truncate text-xs text-muted-foreground">
                {caps.map((c) => c.name).join(' / ')}
              </span>
            </div>
            {e.modelReady === false && (
              <div className="mt-0.5 pl-4">
                <span className="rounded-full border border-border bg-muted px-1.5 py-px text-[10px] text-muted-foreground">
                  {t('modelNotReady')}
                </span>
              </div>
            )}
          </button>
        )
      })}
    </div>
  )
}
