import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { api } from '@/api/client'
import type { DeviceResponse, ModuleResponse } from '@/api/types'
import { PageContainer } from '@/components/layout/page-container'
import { SegmentedTabs } from '@/components/shared/segmented-tabs'
import { categoryVisual } from '@/components/shared/pipeline-node'
import { categoryLabel } from '@/lib/constants'
import {
  CapabilityCatalog,
  type CatalogEntry,
} from '@/components/quick-run/capability-catalog'
import { RunWorkbench } from '@/components/quick-run/run-workbench'
import { RunTaskCard } from '@/components/quick-run/run-task-card'

/** 模块状态轮询间隔（毫秒），与任务中心一致 */
const POLL_INTERVAL = 5000

/** 分类 chips 展示顺序（与管线节点面板分组逻辑对齐：直观功能序）
 *  未知类别归入尾部、以「其他」视觉兜底 */
const CATEGORY_ORDER = [
  'asr',
  'tts',
  'denoise',
  'audio',
  'ocr',
  'image',
  'video',
  'translate',
  'llm',
  'custom',
]

/** 模型就绪判定（与模块页 isReadyStatus 同口径） */
function isReadyStatus(status: string | undefined | null): boolean {
  return (status ?? '').trim().toLowerCase() === 'ready'
}

/**
 * 快速调用页（/run，QUICK_RUN_PLAN D1）：
 * 能力目录（全部已装模块的能力按 category 聚合）+ 右侧工作台 +
 * 页内会话任务区。每次提交即一等公民任务，任务中心可追踪。
 */
export function RunPage() {
  const { t } = useTranslation('run')
  const { t: tModels } = useTranslation('models')

  const [modules, setModules] = useState<ModuleResponse[]>([])
  /** module_id → (model_id → status)，用于激活变体就绪 join */
  const [modelStatusMap, setModelStatusMap] = useState<
    Map<string, Map<string, string>>
  >(new Map())
  const [idleMinutes, setIdleMinutes] = useState<number | null>(null)
  const [devices, setDevices] = useState<DeviceResponse[]>([])
  const [category, setCategory] = useState('')
  const [selectedKey, setSelectedKey] = useState<string | null>(null)
  const [sessionTasks, setSessionTasks] = useState<string[]>([])
  const sessionRef = useRef<HTMLDivElement | null>(null)

  // 首次加载：config（空闲阈值）+ 模型状态；模块列表由轮询覆盖
  useEffect(() => {
    let cancelled = false
    api
      .getConfig()
      .then((cfg) => {
        if (cancelled) return
        const secs = cfg.modules?.idle_timeout_secs
        setIdleMinutes(
          typeof secs === 'number' && secs > 0 ? Math.round(secs / 60) : 0,
        )
      })
      .catch(() => {})
    api
      .models()
      .then((resp) => {
        if (cancelled) return
        const map = new Map<string, Map<string, string>>()
        for (const group of resp.modules) {
          const inner = new Map<string, string>()
          for (const m of group.models) inner.set(m.model_id, m.status)
          map.set(group.module_id, inner)
        }
        setModelStatusMap(map)
      })
      .catch(() => {})
    api
      .devices()
      .then((list) => !cancelled && setDevices(list))
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [])

  // 模块状态轮询（目录状态点 + 提交后自动拉起过程可见）
  useEffect(() => {
    let cancelled = false
    const load = () =>
      api
        .modules()
        .then((list) => !cancelled && setModules(list))
        .catch(() => {})
    void load()
    const timer = window.setInterval(load, POLL_INTERVAL)
    return () => {
      cancelled = true
      window.clearInterval(timer)
    }
  }, [])

  /** 能力目录条目（模块 × 能力扁平化） */
  const entries = useMemo<CatalogEntry[]>(() => {
    const out: CatalogEntry[] = []
    for (const mod of modules) {
      for (const cap of mod.capabilities ?? []) {
        const variant = mod.active_model_id ?? null
        const statusMap = variant ? modelStatusMap.get(mod.id) : undefined
        out.push({
          key: `${mod.id}::${cap.name}`,
          moduleId: mod.id,
          moduleName: mod.name,
          category: (mod.category || 'custom').trim().toLowerCase(),
          capability: cap,
          serviceStatus: mod.service_status,
          device: mod.device ?? null,
          backends: (mod.backends ?? []).map((b) => b.toLowerCase()),
          modelReady:
            variant === null
              ? null
              : statusMap
                ? isReadyStatus(statusMap.get(variant))
                : null,
        })
      }
    }
    const rank = (c: string) => {
      const i = CATEGORY_ORDER.indexOf(c)
      return i === -1 ? CATEGORY_ORDER.length : i
    }
    out.sort(
      (a, b) =>
        rank(a.category) - rank(b.category) ||
        a.moduleName.localeCompare(b.moduleName) ||
        a.capability.name.localeCompare(b.capability.name),
    )
    return out
  }, [modules, modelStatusMap])

interface CategoryChip {
  value: string
  label: ReactNode
  count: number
  tone?: string
}

/** 分类 chip 构造：工程键 → 直观标签 + 管线面板同款图标/配色 */
function chipOf(value: string, count: number, label: ReactNode): CategoryChip {
  const visual = categoryVisual(value)
  const Icon = visual.icon
  return {
    value,
    count,
    label: value === '' ? label : (
      <span className="inline-flex items-center gap-1.5">
        <Icon className="size-3.5" aria-hidden />
        {label}
      </span>
    ),
    // 具体类别才带 accent（「全部」保持中性主色）
    // tone 仅取 text-* 部分（bg 由 SegmentedTabs 激活态主色铺垫）
    tone: value === '' ? undefined : visual.accent.split(' ')[1],
  }
}

/** 分类 chips（与管线节点面板同款直观分类：图标 + 中文标签 + 计数） */
const categories = useMemo<CategoryChip[]>(() => {
  const counts = new Map<string, number>()
  for (const e of entries) counts.set(e.category, (counts.get(e.category) ?? 0) + 1)
  const ordered = [
    ...CATEGORY_ORDER.filter((c) => counts.has(c)),
    ...[...counts.keys()].filter((c) => !CATEGORY_ORDER.includes(c)).sort(),
  ]
  return [
    chipOf('', entries.length, t('categoryAll')),
    ...ordered.map((c) => chipOf(c, counts.get(c) ?? 0, categoryLabel(c))),
  ]
}, [entries, t])

  const filtered = useMemo(
    () => (category ? entries.filter((e) => e.category === category) : entries),
    [entries, category],
  )

  // 选中项兜底：无选中 / 选中被过滤掉时取过滤后第一项
  useEffect(() => {
    if (
      filtered.length > 0 &&
      (!selectedKey || !filtered.some((e) => e.key === selectedKey))
    ) {
      setSelectedKey(filtered[0].key)
    }
  }, [filtered, selectedKey])

  const selected = useMemo(
    () => filtered.find((e) => e.key === selectedKey) ?? null,
    [filtered, selectedKey],
  )

  const handleSubmitted = useCallback((taskId: string) => {
    setSessionTasks((prev) => (prev.includes(taskId) ? prev : [...prev, taskId]))
    // 提交后模块进入拉起流程，立即刷新一次状态点
    api
      .modules()
      .then(setModules)
      .catch(() => {})
    window.setTimeout(
      () => sessionRef.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' }),
      50,
    )
  }, [])

  return (
    <PageContainer title={t('title')} description={t('description')}>
      <div className="space-y-6">
        {/* 空闲自动下线提示（D5） */}
        <p className="text-xs text-muted-foreground">
          {idleMinutes !== null && idleMinutes > 0
            ? t('idleHint', { minutes: idleMinutes })
            : tModels('idleHintAlways')}
        </p>

        <div className="flex flex-col gap-4 lg:flex-row">
          {/* 左栏：分类筛选 + 能力目录 */}
          <aside className="w-full shrink-0 space-y-3 lg:w-80">
            <SegmentedTabs
              items={categories}
              value={category}
              onChange={setCategory}
              ariaLabel={t('categoryAll')}
            />
            <CapabilityCatalog
              entries={filtered}
              selectedKey={selectedKey}
              onSelect={setSelectedKey}
            />
          </aside>

          {/* 右侧工作台 */}
          <section className="min-w-0 flex-1">
            {selected ? (
              <RunWorkbench
                entry={selected}
                devices={devices}
                idleMinutes={idleMinutes}
                onSubmitted={handleSubmitted}
                onModuleChanged={() => {
                  api
                    .modules()
                    .then(setModules)
                    .catch(() => {})
                }}
              />
            ) : (
              <div className="rounded-xl border border-dashed border-border p-8 text-center text-sm text-muted-foreground">
                {t('noCapabilities')}
              </div>
            )}
          </section>
        </div>

        {/* 会话任务区 */}
        {sessionTasks.length > 0 && (
          <section ref={sessionRef} className="space-y-3">
            <div className="grid gap-3 lg:grid-cols-2">
              {sessionTasks.map((id) => (
                <RunTaskCard
                  key={id}
                  taskId={id}
                  onDismiss={() =>
                    setSessionTasks((prev) => prev.filter((x) => x !== id))
                  }
                />
              ))}
            </div>
          </section>
        )}
      </div>
    </PageContainer>
  )
}
