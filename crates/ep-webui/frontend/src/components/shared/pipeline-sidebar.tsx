import { useCallback, useEffect, useMemo, useState } from 'react'
import type { DragEvent, KeyboardEvent } from 'react'
import { CircleAlert, GripVertical, RefreshCw, Search, Wrench, X } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { api } from '@/api/client'
import type { CapabilityDecl, ModuleResponse } from '@/api/types'
import { categoryLabel, statusMeta } from '@/lib/constants'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { ScrollArea } from '@/components/ui/scroll-area'
import { BUILTIN_LIST, DRAG_MIME, categoryVisual } from '@/components/shared/pipeline-node'
import type { DragPayload } from '@/components/shared/pipeline-node'

/**
 * ModuleResponse.capabilities 的能力裸名列表（null 安全）。
 * B5 过渡期 / 无能力模块返回空数组，消费方展示兜底文案。
 */
function capabilityNames(module_: ModuleResponse): string[] {
  const caps: CapabilityDecl[] | null | undefined = module_.capabilities
  if (!Array.isArray(caps)) return []
  return caps
    .filter((c) => c && typeof c.name === 'string' && c.name.trim())
    .map((c) => c.name.trim())
}

function startDrag(event: DragEvent<HTMLDivElement>, payload: DragPayload) {
  event.dataTransfer.setData(DRAG_MIME, JSON.stringify(payload))
  event.dataTransfer.effectAllowed = 'move'
}

interface PaletteItemProps {
  icon: React.ReactNode
  title: string
  subtitle: string
  accent: string
  trailing?: React.ReactNode
  payload: DragPayload
  onAdd: (payload: DragPayload) => void
}

function PaletteItem({
  icon,
  title,
  subtitle,
  accent,
  trailing,
  payload,
  onAdd,
}: PaletteItemProps) {
  const { t } = useTranslation('components')
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault()
      onAdd(payload)
    }
  }
  return (
    <div
      draggable
      role="button"
      tabIndex={0}
      aria-label={t('pipelineSidebar.addNodeAria', { title })}
      onDragStart={(e) => startDrag(e, payload)}
      onClick={() => onAdd(payload)}
      onKeyDown={handleKeyDown}
      title={t('pipelineSidebar.addHint')}
      className="group flex cursor-grab items-center gap-2.5 rounded-md border border-transparent px-2 py-1.5 transition-all duration-150 hover:border-border hover:bg-accent active:cursor-grabbing active:scale-[0.98]"
    >
      <span
        className={cn(
          'flex h-7 w-7 shrink-0 items-center justify-center rounded-md transition-transform duration-150 group-hover:scale-110',
          accent,
        )}
      >
        {icon}
      </span>
      <div className="min-w-0 flex-1">
        <p className="truncate text-[13px] font-medium leading-tight">{title}</p>
        <p className="truncate text-[11px] text-muted-foreground">{subtitle}</p>
      </div>
      {trailing}
      <GripVertical className="h-3.5 w-3.5 shrink-0 text-muted-foreground opacity-0 transition-opacity duration-150 group-hover:opacity-70" />
    </div>
  )
}

function SectionTitle({ children, count }: { children: React.ReactNode; count?: number }) {
  return (
    <div className="flex items-center gap-1.5 px-2 pb-1.5 pt-4 first:pt-1">
      <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
        {children}
      </span>
      {count !== undefined && (
        <span className="rounded-full bg-muted px-1.5 py-px font-mono text-[10px] text-muted-foreground">
          {count}
        </span>
      )}
      <span className="h-px flex-1 bg-border" />
    </div>
  )
}

interface PipelineSidebarProps {
  /** 点击（或键盘确认）节点库项时添加节点到画布 */
  onAdd: (payload: DragPayload) => void
  /** 提供时在头部显示关闭按钮（窄屏抽屉模式） */
  onClose?: () => void
  /** 附加布局类（窄屏 overlay 定位等） */
  className?: string
}

/** 管线节点库：内置节点 + 按分类分组的模块（可点击 / 拖入画布） */
export function PipelineSidebar({ onAdd, onClose, className }: PipelineSidebarProps) {
  // i18n.language 进入过滤 memo 依赖：语言切换后按新语言文案重新匹配
  const { t, i18n } = useTranslation('components')
  const [modules, setModules] = useState<ModuleResponse[] | null>(null)
  const [failed, setFailed] = useState(false)
  const [filter, setFilter] = useState('')

  const load = useCallback(() => {
    setFailed(false)
    setModules(null)
    api
      .modules()
      .then((list) => setModules(list))
      .catch(() => setFailed(true))
  }, [])

  useEffect(() => {
    load()
  }, [load])

  const query = filter.trim().toLowerCase()

  const builtinItems = useMemo(
    () =>
      BUILTIN_LIST.filter(
        (b) =>
          !query ||
          b.label.toLowerCase().includes(query) ||
          b.description.toLowerCase().includes(query),
      ),
    [query, i18n.language],
  )

  const groupedModules = useMemo(() => {
    if (!modules) return null
    const matched = modules.filter(
      (m) =>
        !query ||
        m.name.toLowerCase().includes(query) ||
        m.id.toLowerCase().includes(query) ||
        m.description.toLowerCase().includes(query) ||
        categoryLabel(m.category).toLowerCase().includes(query) ||
        capabilityNames(m).some((name) => name.toLowerCase().includes(query)),
    )
    const groups = new Map<string, ModuleResponse[]>()
    for (const m of matched) {
      const list = groups.get(m.category) ?? []
      list.push(m)
      groups.set(m.category, list)
    }
    return [...groups.entries()]
  }, [modules, query, i18n.language])

  // §6.7：external_api 不进 palette —— LLM 接入统一走 llm builtin 项
  return (
    <aside
      className={cn('flex h-full w-60 shrink-0 flex-col border-r border-border bg-card', className)}
    >
      <div className="shrink-0 space-y-2.5 border-b border-border p-3">
        <div className="flex items-center justify-between">
          <span className="text-sm font-semibold">{t('pipelineSidebar.title')}</span>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="icon-xs"
              onClick={load}
              title={t('pipelineSidebar.refreshModules')}
              aria-label={t('pipelineSidebar.refreshModules')}
            >
              <RefreshCw
                className={cn('h-3 w-3', modules === null && !failed && 'animate-spin')}
              />
            </Button>
            {onClose && (
              <Button
                variant="ghost"
                size="icon-xs"
                onClick={onClose}
                title={t('pipelineSidebar.closeLibrary')}
                aria-label={t('pipelineSidebar.closeLibrary')}
              >
                <X className="h-3 w-3" />
              </Button>
            )}
          </div>
        </div>
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder={t('pipelineSidebar.searchPlaceholder')}
            aria-label={t('pipelineSidebar.searchLabel')}
            className="h-8 w-full rounded-md border border-input bg-transparent pl-8 pr-2 text-xs outline-none transition-colors placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
          />
        </div>
      </div>

      <ScrollArea className="flex-1">
        <div className="p-2 pb-6">
          <SectionTitle>{t('pipelineSidebar.builtinSection')}</SectionTitle>
          <div className="space-y-0.5">
            {builtinItems.map((b) => (
              <PaletteItem
                key={b.kind}
                icon={<b.icon className="h-4 w-4" />}
                title={b.label}
                subtitle={b.description}
                accent={b.accent}
                payload={{ nodeType: 'builtin', builtin: b.kind }}
                onAdd={onAdd}
              />
            ))}
          </div>

          <SectionTitle count={modules?.length}>{t('common:label.module')}</SectionTitle>
          {modules === null && !failed && (
            <div className="space-y-1.5 px-2 py-1">
              {Array.from({ length: 4 }).map((_, i) => (
                <div key={i} className="flex items-center gap-2.5">
                  <Skeleton className="h-7 w-7 rounded-md" />
                  <div className="flex-1 space-y-1">
                    <Skeleton className="h-3 w-3/4" />
                    <Skeleton className="h-2.5 w-1/2" />
                  </div>
                </div>
              ))}
            </div>
          )}

          {failed && (
            <div className="mx-1 flex flex-col items-center gap-2 rounded-md border border-dashed border-border px-3 py-5 text-center">
              <CircleAlert className="h-5 w-5 text-status-error" />
              <p className="text-xs text-muted-foreground">
                {t('pipelineSidebar.loadFailed')}
              </p>
              <Button variant="outline" size="xs" onClick={load}>
                {t('common:action.retry')}
              </Button>
            </div>
          )}

          {groupedModules && groupedModules.length === 0 && (
            <div className="flex flex-col items-center gap-1.5 px-3 py-5 text-center">
              <Wrench className="h-4 w-4 text-muted-foreground" />
              <p className="text-xs text-muted-foreground">
                {query
                  ? t('pipelineSidebar.noMatches')
                  : t('pipelineSidebar.noModules')}
              </p>
            </div>
          )}

          {groupedModules?.map(([category, list]) => (
            <div key={category} className="mb-1">
              <p className="px-2 pb-1 pt-2 text-[11px] font-medium text-muted-foreground">
                {categoryLabel(category)}
              </p>
              <div className="space-y-0.5">
                {list.map((m) => {
                  const visual = categoryVisual(m.category)
                  const Icon = visual.icon
                  const st = statusMeta(m.status)
                  // P0-1：能力裸名随载荷传递，节点创建完全数据驱动
                  const caps = capabilityNames(m)
                  const subtitle =
                    caps.length > 0
                      ? `${m.id} · ${caps.join(' / ')}`
                      : `${m.id} · ${t('pipelineSidebar.noCapabilities', { defaultValue: '未声明能力' })}`
                  return (
                    <PaletteItem
                      key={m.id}
                      icon={<Icon className="h-4 w-4" />}
                      title={m.name}
                      subtitle={subtitle}
                      accent={visual.accent}
                      trailing={
                        <span
                          title={t('pipelineSidebar.moduleStatusTitle', { status: st.label })}
                          className={cn('h-1.5 w-1.5 shrink-0 rounded-full', st.dot)}
                        />
                      }
                      payload={{
                        nodeType: 'module',
                        moduleId: m.id,
                        moduleName: m.name,
                        moduleVersion: m.version,
                        category: m.category,
                        capabilities: m.capabilities ?? [],
                      }}
                      onAdd={onAdd}
                    />
                  )
                })}
              </div>
            </div>
          ))}
        </div>
      </ScrollArea>

      <div className="shrink-0 border-t border-border px-3 py-2.5 text-[11px] text-muted-foreground">
        {t('pipelineSidebar.footerHint')}
      </div>
    </aside>
  )
}
