import { useCallback, useEffect, useMemo, useState } from 'react'
import type { DragEvent } from 'react'
import { CircleAlert, Globe, GripVertical, RefreshCw, Search, Wrench } from 'lucide-react'
import { api } from '@/api/client'
import type { ModuleResponse } from '@/api/types'
import { categoryLabel, statusMeta } from '@/lib/constants'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { ScrollArea } from '@/components/ui/scroll-area'
import { BUILTIN_LIST, DRAG_MIME, categoryVisual } from '@/components/shared/pipeline-node'
import type { DragPayload } from '@/components/shared/pipeline-node'

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
}

function PaletteItem({ icon, title, subtitle, accent, trailing, payload }: PaletteItemProps) {
  return (
    <div
      draggable
      onDragStart={(e) => startDrag(e, payload)}
      title="拖拽到右侧画布以添加节点"
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

/** 管线节点库：内置节点 + 按分类分组的模块（可拖入画布） */
export function PipelineSidebar() {
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
    [query],
  )

  const groupedModules = useMemo(() => {
    if (!modules) return null
    const matched = modules.filter(
      (m) =>
        !query ||
        m.name.toLowerCase().includes(query) ||
        m.id.toLowerCase().includes(query) ||
        m.description.toLowerCase().includes(query) ||
        categoryLabel(m.category).toLowerCase().includes(query),
    )
    const groups = new Map<string, ModuleResponse[]>()
    for (const m of matched) {
      const list = groups.get(m.category) ?? []
      list.push(m)
      groups.set(m.category, list)
    }
    return [...groups.entries()]
  }, [modules, query])

  const externalVisible =
    !query || '外部 api external http 接口'.includes(query) || 'api'.includes(query)

  return (
    <aside className="flex w-60 shrink-0 flex-col border-r border-border bg-card">
      <div className="shrink-0 space-y-2.5 border-b border-border p-3">
        <div className="flex items-center justify-between">
          <span className="text-sm font-semibold">节点库</span>
          <Button
            variant="ghost"
            size="icon-xs"
            onClick={load}
            title="刷新模块列表"
            aria-label="刷新模块列表"
          >
            <RefreshCw className={cn('h-3 w-3', modules === null && !failed && 'animate-spin')} />
          </Button>
        </div>
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="搜索节点…"
            aria-label="搜索节点"
            className="h-8 w-full rounded-md border border-input bg-transparent pl-8 pr-2 text-xs outline-none transition-colors placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
          />
        </div>
      </div>

      <ScrollArea className="flex-1">
        <div className="p-2 pb-6">
          <SectionTitle>内置节点</SectionTitle>
          <div className="space-y-0.5">
            {builtinItems.map((b) => (
              <PaletteItem
                key={b.kind}
                icon={<b.icon className="h-4 w-4" />}
                title={b.label}
                subtitle={b.description}
                accent={b.accent}
                payload={{ nodeType: 'builtin', builtin: b.kind }}
              />
            ))}
            {externalVisible && (
              <PaletteItem
                icon={<Globe className="h-4 w-4" />}
                title="外部 API"
                subtitle="调用外部 HTTP 接口"
                accent="bg-cyan-500/15 text-cyan-400"
                payload={{ nodeType: 'external' }}
              />
            )}
          </div>

          <SectionTitle count={modules?.length}>模块</SectionTitle>
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
              <p className="text-xs text-muted-foreground">模块列表加载失败</p>
              <Button variant="outline" size="xs" onClick={load}>
                重试
              </Button>
            </div>
          )}

          {groupedModules && groupedModules.length === 0 && (
            <div className="flex flex-col items-center gap-1.5 px-3 py-5 text-center">
              <Wrench className="h-4 w-4 text-muted-foreground" />
              <p className="text-xs text-muted-foreground">
                {query ? '没有匹配的节点' : '暂无已安装模块'}
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
                  return (
                    <PaletteItem
                      key={m.id}
                      icon={<Icon className="h-4 w-4" />}
                      title={m.name}
                      subtitle={`${m.id} · v${m.version}`}
                      accent={visual.accent}
                      trailing={
                        <span
                          title={`模块状态：${st.label}`}
                          className={cn('h-1.5 w-1.5 shrink-0 rounded-full', st.dot)}
                        />
                      }
                      payload={{
                        nodeType: 'module',
                        moduleId: m.id,
                        moduleName: m.name,
                        moduleVersion: m.version,
                        category: m.category,
                      }}
                    />
                  )
                })}
              </div>
            </div>
          ))}
        </div>
      </ScrollArea>

      <div className="shrink-0 border-t border-border px-3 py-2.5 text-[11px] text-muted-foreground">
        拖拽节点到右侧画布以添加
      </div>
    </aside>
  )
}
