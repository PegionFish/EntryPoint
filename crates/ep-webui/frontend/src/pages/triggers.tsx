import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  BellRing,
  Clock,
  Copy,
  History,
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  Trash2,
  Webhook,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import type { TFunction } from 'i18next'
import { toast } from 'sonner'
import { api } from '@/api/client'
import type {
  ModuleResponse,
  PipelineNodeSpec,
  PipelineSummary,
  ScheduleListEntry,
  UnifiedEvent,
  WatcherConflictPolicy,
  WatcherDirectAction,
  WatchRule,
  WatchRuleInput,
  WebhookEntry,
  WebhookTargetInput,
} from '@/api/types'
import { PageContainer } from '@/components/layout/page-container'
import { SegmentedTabs } from '@/components/shared/segmented-tabs'
import { StoreBrowserDialog } from '@/components/shared/store-browser'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { cn } from '@/lib/utils'

/** 历史查询条数（§5.3：GET /api/events?rule=<id>&limit=100，倒序） */
const HISTORY_LIMIT = 100

/** 最近触发速览条数（§5.1 recent 环形缓冲上限） */
const RECENT_PREVIEW_MAX = 5

/** 动作模式（对话框表单视角：直接模式拆为 仅归档 / 模块 两档） */
type RuleMode = 'direct-archive' | 'direct-module' | 'pipeline'

/** 规则编辑表单状态（extensions/exclude 以原始文本持有，保存时归一） */
interface RuleForm {
  name: string
  watch_dir: string
  extensionsText: string
  excludeText: string
  recursive: boolean
  include_modified: boolean
  stability_secs: string
  backfill: boolean
  dedup_enabled: boolean
  dedup_max_mb: string
  enabled: boolean
  mode: RuleMode
  moduleId: string
  capability: string
  pipelineId: string
  inputNode: string
  dest_dir: string
  name_template: string
  on_conflict: WatcherConflictPolicy
}

const DEFAULT_FORM: RuleForm = {
  name: '',
  watch_dir: '',
  extensionsText: '',
  excludeText: '',
  recursive: false,
  include_modified: false,
  stability_secs: '30',
  backfill: false,
  dedup_enabled: true,
  dedup_max_mb: '64',
  enabled: true,
  mode: 'direct-archive',
  moduleId: '',
  capability: '',
  pipelineId: '',
  inputNode: '',
  dest_dir: '',
  name_template: '{name}.{ext}',
  on_conflict: 'suffix',
}

/** 编辑表单 → 规则（读取侧兼容 DirectKind PascalCase/小写两种 serde 变体名） */
function ruleMode(rule: WatchRule): RuleMode {
  const kind = (rule.direct?.kind?.type ?? '').toLowerCase()
  if (kind === 'module') return 'direct-module'
  if (kind === 'archive') return 'direct-archive'
  if (rule.pipeline) return 'pipeline'
  return 'direct-archive'
}

/** 扩展名输入归一：按逗号/分号/空白切分 → 去点 → 小写 → 去重去空 */
function parseExtensions(text: string): string[] {
  const list = text
    .split(/[，,;\s]+/)
    .map((s) => s.replace(/^\./, '').trim().toLowerCase())
    .filter(Boolean)
  return Array.from(new Set(list))
}

/**
 * 排除 glob 输入归一（T-51）：按逗号/分号/换行切分 → 去空白 → 去重去空，
 * 保持书写顺序。glob 大小写保留（匹配期按平台折叠），此处不归一大小写。
 */
function parseExclude(text: string): string[] {
  const list = text
    .split(/[，,;\n]+/)
    .map((s) => s.trim())
    .filter(Boolean)
  return Array.from(new Set(list))
}

/** 去重哈希阈值（MB）归一：钳制 1–4096（与服务端校验同口径），默认 64 */
function parseDedupMaxMb(text: string): number {
  const n = Number(text)
  if (!Number.isFinite(n)) return 64
  return Math.min(4096, Math.max(1, Math.floor(n)))
}

/**
 * 平台感知的绝对路径判定（T-04）。
 *
 * 前端只做语法层放行，最终以服务端 `Path::is_absolute()` 为准：
 * - POSIX：`/` 开头（如 `/data/watch`）
 * - Windows 盘符：`C:\` 或 `C:/` 开头（如 `F:\watch`、`F:/watch`）
 * - UNC 共享：`\\` 开头（如 `\\nas\share`）
 * 其余（相对路径、空串）一律非法。
 */
function isAbsolutePath(path: string): boolean {
  return (
    path.startsWith('/') ||
    /^[A-Za-z]:[\\/]/.test(path) ||
    path.startsWith('\\\\')
  )
}

/**
 * 路径归一键（T-25）：与后端 `normalize_path_key` 同款语义——统一分隔符、
 * 剥离 `\\?\` 前缀、去尾部分隔符；Windows 形状路径（盘符 / UNC，即
 * isAbsolutePath 判真的形状）大小写不敏感。仅用于重叠快捷提示，
 * 最终以服务端校验为准。
 */
function normalizePathKey(path: string): string {
  const stripped = path.trim().replace(/^\\\\\?\\/, '')
  const isWindowsShape = /^[A-Za-z]:[\\/]/.test(stripped) || stripped.startsWith('\\\\')
  const unified = stripped.replace(/[\\/]+/g, '/')
  const folded = isWindowsShape ? unified.toLowerCase() : unified
  return folded !== '/' ? folded.replace(/\/+$/, '') : folded
}

/**
 * 目录重叠判定（T-25）：相等或一方为另一方的路径前缀（组件级）。
 * 与服务端第 8 步校验同语义；产物回流监控目录会自我触发，创建期即提示。
 */
function pathsOverlap(a: string, b: string): boolean {
  const na = normalizePathKey(a)
  const nb = normalizePathKey(b)
  if (!na || !nb) return false
  return na === nb || na.startsWith(`${nb}/`) || nb.startsWith(`${na}/`)
}

/** 从 apiFetch 抛出的 `API <status>: <body>` 错误中提取后端 error 字段 */
function backendErrorMessage(err: unknown): string {
  if (!(err instanceof Error)) return String(err)
  const idx = err.message.indexOf(': ')
  const body = idx === -1 ? err.message : err.message.slice(idx + 2)
  try {
    const parsed = JSON.parse(body) as { error?: unknown }
    if (parsed && typeof parsed.error === 'string' && parsed.error.trim()) {
      return parsed.error
    }
  } catch {
    // 非 JSON body，原样展示
  }
  return body.trim() || err.message
}

/**
 * 后端校验错误统一返回 apiCore.watcher.* i18n 键（§5.3）。
 * 键形状（如 "apiCore.watcher.nameRequired"）转前端翻译；
 * 已本地化的普通文案或网络错误原样展示。
 */
function displayBackendError(err: unknown, t: TFunction): string {
  const msg = backendErrorMessage(err)
  const m = /^apiCore\.(.+)$/.exec(msg.trim())
  if (m) return t(`apiCore:${m[1]}`, { defaultValue: msg })
  return msg
}

/** 文件路径 → 基名（仅用于速览/列表截断展示） */
function basename(path: string): string {
  const idx = path.lastIndexOf('/')
  return idx === -1 ? path : path.slice(idx + 1)
}

/** recent / 历史条目状态 → 状态点配色（index.css 语义色令牌） */
function statusDotClass(status: string | null | undefined): string {
  switch ((status ?? '').trim().toLowerCase()) {
    case 'submitted':
    case 'archive_done':
    case 'completed':
      return 'bg-status-running'
    case 'rejected':
    case 'failed':
      return 'bg-status-error'
    case 'archive_skipped':
    case 'dedup_skipped':
    case 'cancelled':
      return 'bg-status-preparing'
    default:
      return 'bg-muted-foreground'
  }
}

/** epoch 秒 → 本地时间文本 */
function formatTs(ts: number, language: string): string {
  return new Date(ts * 1000).toLocaleString(language, {
    dateStyle: 'short',
    timeStyle: 'medium',
  })
}

/** 列表页动作模式展示文案（含模块能力 / 管线 id 附加信息） */
function modeSummary(rule: WatchRule, t: TFunction): string {
  const kind = (rule.direct?.kind?.type ?? '').toLowerCase()
  if (kind === 'archive') return t('mode.directArchive')
  if (kind === 'module') {
    const k = rule.direct?.kind as { module_id?: string; capability?: string } | undefined
    const cap = k?.capability || k?.module_id || ''
    return cap ? `${t('mode.directModule')} · ${cap}` : t('mode.directModule')
  }
  if (rule.pipeline) {
    return `${t('mode.pipeline')} · ${rule.pipeline.pipeline_id}/${rule.pipeline.input_node}`
  }
  return t('mode.unknown')
}

/** 编辑表单 → POST/PUT 请求体（§5.1/§5.3：direct 与 pipeline 恰好一个） */
function formToInput(form: RuleForm): WatchRuleInput {
  const extensions = parseExtensions(form.extensionsText)
  const stability = Math.max(
    5,
    Number.isFinite(Number(form.stability_secs))
      ? Math.floor(Number(form.stability_secs))
      : 30,
  )
  let direct: WatcherDirectAction | null = null
  let pipeline: { pipeline_id: string; input_node: string } | null = null
  let output: WatchRuleInput['output'] = null
  if (form.mode === 'direct-archive' || form.mode === 'direct-module') {
    direct =
      form.mode === 'direct-archive'
        ? { kind: { type: 'Archive' } }
        : { kind: { type: 'Module', module_id: form.moduleId, capability: form.capability } }
    output = {
      dest_dir: form.dest_dir.trim(),
      name_template: form.name_template.trim() || '{name}.{ext}',
      on_conflict: form.on_conflict,
    }
  } else {
    pipeline = { pipeline_id: form.pipelineId, input_node: form.inputNode }
  }
  return {
    name: form.name.trim(),
    enabled: form.enabled,
    watch_dir: form.watch_dir.trim(),
    recursive: form.recursive,
    extensions,
    exclude: parseExclude(form.excludeText),
    dedup: {
      enabled: form.dedup_enabled,
      max_hash_mb: parseDedupMaxMb(form.dedup_max_mb),
    },
    include_modified: form.include_modified,
    stability_secs: stability,
    backfill: form.backfill,
    direct,
    pipeline,
    output,
  }
}

// ── 规则编辑对话框（对应 §5.3 校验链的字段全覆盖） ──────────────────────

function RuleDialog({
  open,
  onOpenChange,
  rule,
  onSaved,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  rule: WatchRule | null
  onSaved: () => void
}) {
  const { t, i18n } = useTranslation('triggers')
  const [form, setForm] = useState<RuleForm>(DEFAULT_FORM)
  const [saving, setSaving] = useState(false)
  const [modules, setModules] = useState<ModuleResponse[] | null>(null)
  const [pipelines, setPipelines] = useState<PipelineSummary[] | null>(null)
  const [inputNodes, setInputNodes] = useState<PipelineNodeSpec[] | null>(null)

  const set = <K extends keyof RuleForm>(key: K, value: RuleForm[K]) =>
    setForm((prev) => ({ ...prev, [key]: value }))

  // 打开时初始化表单（编辑 → 预填；新建 → 默认值）并加载下拉数据源
  useEffect(() => {
    if (!open) return
    if (rule) {
      const direct = rule.direct
      const kind = (direct?.kind?.type ?? '').toLowerCase()
      const moduleDirect =
        kind === 'module' ? (direct?.kind as { module_id?: string; capability?: string }) : undefined
      setForm({
        name: rule.name,
        watch_dir: rule.watch_dir,
        extensionsText: (rule.extensions ?? []).join(', '),
        excludeText: (rule.exclude ?? []).join(', '),
        recursive: rule.recursive ?? false,
        include_modified: rule.include_modified ?? false,
        stability_secs: String(rule.stability_secs ?? 30),
        backfill: rule.backfill ?? false,
        dedup_enabled: rule.dedup?.enabled ?? true,
        dedup_max_mb: String(rule.dedup?.max_hash_mb ?? 64),
        enabled: rule.enabled,
        mode: ruleMode(rule),
        moduleId: moduleDirect?.module_id ?? '',
        capability: moduleDirect?.capability ?? '',
        pipelineId: rule.pipeline?.pipeline_id ?? '',
        inputNode: rule.pipeline?.input_node ?? '',
        dest_dir: rule.output?.dest_dir ?? '',
        name_template: rule.output?.name_template ?? '{name}.{ext}',
        on_conflict: rule.output?.on_conflict ?? 'suffix',
      })
    } else {
      setForm(DEFAULT_FORM)
    }
    setModules(null)
    setPipelines(null)
    setInputNodes(null)
    api
      .modules()
      .then((list) =>
        setModules(list.filter((m) => (m.capabilities ?? []).length > 0)),
      )
      .catch(() => setModules([]))
    api
      .listPipelines()
      .then(setPipelines)
      .catch(() => setPipelines([]))
  }, [open, rule])

  // 管线模式下按所选管线拉取 spec，解析 file_input 注入节点（D3：始终手动选择）
  useEffect(() => {
    if (!open || form.mode !== 'pipeline' || !form.pipelineId) {
      setInputNodes(null)
      return
    }
    let cancelled = false
    setInputNodes(null)
    api
      .getPipeline(form.pipelineId)
      .then((spec) => {
        if (!cancelled) {
          setInputNodes(
            spec.nodes.filter((n) => n.kind === 'builtin' && n.builtin === 'file_input'),
          )
        }
      })
      .catch(() => {
        if (!cancelled) setInputNodes([])
      })
    return () => {
      cancelled = true
    }
  }, [open, form.mode, form.pipelineId])

  const selectedModule = useMemo(
    () => modules?.find((m) => m.id === form.moduleId) ?? null,
    [modules, form.moduleId],
  )

  const handleSave = useCallback(async () => {
    // 客户端快捷校验（与 §5.3 后端校验链对应；最终以服务端错误键为准）
    if (!form.name.trim()) {
      toast.error(t('validation.nameRequired'))
      return
    }
    if (!form.watch_dir.trim()) {
      toast.error(t('validation.watchDirRequired'))
      return
    }
    if (!isAbsolutePath(form.watch_dir.trim())) {
      toast.error(t('validation.watchDirInvalid'))
      return
    }
    if (form.mode === 'direct-module' && (!form.moduleId || !form.capability)) {
      toast.error(t('validation.capabilityRequired'))
      return
    }
    if (form.mode === 'pipeline' && (!form.pipelineId || !form.inputNode)) {
      toast.error(t('validation.pipelineRequired'))
      return
    }
    if (form.mode !== 'pipeline') {
      if (!form.dest_dir.trim()) {
        toast.error(t('validation.destDirRequired'))
        return
      }
      if (!isAbsolutePath(form.dest_dir.trim())) {
        toast.error(t('validation.destDirInvalid'))
        return
      }
      // T-25：监控目录与输出目录重叠 → 产物回流监控目录自我触发，创建期即拒
      if (pathsOverlap(form.watch_dir, form.dest_dir)) {
        toast.error(t('validation.pathOverlap'))
        return
      }
    }
    setSaving(true)
    try {
      const body = formToInput(form)
      if (rule) {
        await api.updateWatcher(rule.id, body)
      } else {
        await api.createWatcher(body)
      }
      toast.success(t('dialog.saveSuccess'))
      onOpenChange(false)
      onSaved()
    } catch (err) {
      toast.error(t('dialog.saveFailed'), {
        description: displayBackendError(err, t),
      })
    } finally {
      setSaving(false)
    }
  }, [form, rule, t, onOpenChange, onSaved])

  const modeItems = [
    { value: 'direct-archive' as RuleMode, label: t('dialog.mode.archiveLabel') },
    { value: 'direct-module' as RuleMode, label: t('dialog.mode.moduleLabel') },
    { value: 'pipeline' as RuleMode, label: t('dialog.mode.pipelineLabel') },
  ]

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            {rule ? t('dialog.editTitle') : t('dialog.createTitle')}
            {rule ? (
              <span className="ml-2 font-mono text-xs text-muted-foreground">
                {rule.id}
              </span>
            ) : null}
          </DialogTitle>
        </DialogHeader>
        <div className="max-h-[70vh] space-y-5 overflow-y-auto py-2 pr-1">
          {/* ── 基本信息 ── */}
          <section className="space-y-3">
            <h3 className="text-sm font-semibold">{t('dialog.section.common')}</h3>
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="space-y-1.5">
                <label className="text-sm font-medium">{t('dialog.name')}</label>
                <Input
                  value={form.name}
                  onChange={(e) => set('name', e.target.value)}
                  placeholder={t('dialog.namePlaceholder')}
                />
              </div>
              <div className="space-y-1.5">
                <label className="text-sm font-medium">
                  {t('dialog.stability')}
                </label>
                <Input
                  type="number"
                  min={5}
                  value={form.stability_secs}
                  onChange={(e) => set('stability_secs', e.target.value)}
                />
                <p className="text-[11px] text-muted-foreground">
                  {t('dialog.stabilityHint')}
                </p>
              </div>
            </div>
            <div className="space-y-1.5">
              <label className="text-sm font-medium">{t('dialog.watchDir')}</label>
              <Input
                value={form.watch_dir}
                onChange={(e) => set('watch_dir', e.target.value)}
                placeholder={t('dialog.watchDirPlaceholder')}
                className="font-mono text-xs"
              />
              <p className="text-[11px] text-muted-foreground">
                {t('dialog.watchDirHint')}
              </p>
            </div>
            <div className="space-y-1.5">
              <label className="text-sm font-medium">
                {t('dialog.extensions')}
              </label>
              <Input
                value={form.extensionsText}
                onChange={(e) => set('extensionsText', e.target.value)}
                placeholder={t('dialog.extensionsPlaceholder')}
              />
              <p className="text-[11px] text-muted-foreground">
                {t('dialog.extensionsHint')}
              </p>
            </div>
            <div className="space-y-1.5">
              <label className="text-sm font-medium">
                {t('dialog.exclude')}
              </label>
              <Input
                value={form.excludeText}
                onChange={(e) => set('excludeText', e.target.value)}
                placeholder={t('dialog.excludePlaceholder')}
                className="font-mono text-xs"
              />
              <p className="text-[11px] text-muted-foreground">
                {t('dialog.excludeHint')}
              </p>
            </div>
            <div className="grid gap-2 sm:grid-cols-2">
              {(
                [
                  ['recursive', 'dialog.recursive', 'dialog.recursiveHint'],
                  [
                    'include_modified',
                    'dialog.includeModified',
                    'dialog.includeModifiedHint',
                  ],
                  ['backfill', 'dialog.backfill', 'dialog.backfillHint'],
                  [
                    'dedup_enabled',
                    'dialog.dedupEnabled',
                    'dialog.dedupEnabledHint',
                  ],
                  ['enabled', 'dialog.enabled', 'dialog.enabledHint'],
                ] as const
              ).map(([key, labelKey, hintKey]) => (
                <div
                  key={key}
                  className="flex items-start justify-between gap-3 rounded-lg border border-border px-3 py-2.5"
                >
                  <div className="min-w-0">
                    <p className="text-sm">{t(labelKey)}</p>
                    <p className="mt-0.5 text-[11px] leading-snug text-muted-foreground">
                      {t(hintKey)}
                    </p>
                  </div>
                  <Switch
                    checked={form[key]}
                    onCheckedChange={(v) => set(key, v)}
                    aria-label={t(labelKey)}
                  />
                </div>
              ))}
            </div>
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="space-y-1.5">
                <label className="text-sm font-medium">
                  {t('dialog.dedupMaxMb')}
                </label>
                <Input
                  type="number"
                  min={1}
                  max={4096}
                  value={form.dedup_max_mb}
                  onChange={(e) => set('dedup_max_mb', e.target.value)}
                  disabled={!form.dedup_enabled}
                />
                <p className="text-[11px] text-muted-foreground">
                  {t('dialog.dedupMaxMbHint')}
                </p>
              </div>
            </div>
          </section>

          {/* ── 动作模式 ── */}
          <section className="space-y-3">
            <h3 className="text-sm font-semibold">{t('dialog.section.action')}</h3>
            <div className="space-y-1.5">
              <label className="text-sm font-medium">{t('dialog.modeLabel')}</label>
              <SegmentedTabs
                items={modeItems}
                value={form.mode}
                onChange={(v) => set('mode', v)}
                ariaLabel={t('dialog.modeLabel')}
                className="w-fit"
              />
              <p className="text-[11px] text-muted-foreground">
                {form.mode === 'direct-archive'
                  ? t('dialog.mode.archiveDesc')
                  : form.mode === 'direct-module'
                    ? t('dialog.mode.moduleDesc')
                    : t('dialog.mode.pipelineDesc')}
              </p>
            </div>
            {form.mode === 'direct-module' ? (
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="space-y-1.5">
                  <label className="text-sm font-medium">{t('dialog.module')}</label>
                  <Select
                    value={form.moduleId || undefined}
                    onValueChange={(v) => {
                      set('moduleId', v)
                      set('capability', '')
                    }}
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue placeholder={t('dialog.modulePlaceholder')} />
                    </SelectTrigger>
                    <SelectContent>
                      {(modules ?? []).map((m) => (
                        <SelectItem key={m.id} value={m.id}>
                          {m.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {modules !== null && modules.length === 0 ? (
                    <p className="text-[11px] text-muted-foreground">
                      {t('dialog.moduleNone')}
                    </p>
                  ) : null}
                </div>
                <div className="space-y-1.5">
                  <label className="text-sm font-medium">
                    {t('dialog.capability')}
                  </label>
                  <Select
                    value={form.capability || undefined}
                    onValueChange={(v) => set('capability', v)}
                    disabled={!form.moduleId}
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue
                        placeholder={t('dialog.capabilityPlaceholder')}
                      />
                    </SelectTrigger>
                    <SelectContent>
                      {(selectedModule?.capabilities ?? []).map((cap) => (
                        <SelectItem key={cap.name} value={cap.name}>
                          {cap.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              </div>
            ) : null}
            {form.mode === 'pipeline' ? (
              <div className="space-y-3">
                <div className="space-y-1.5">
                  <label className="text-sm font-medium">
                    {t('dialog.pipeline')}
                  </label>
                  <Select
                    value={form.pipelineId || undefined}
                    onValueChange={(v) => {
                      set('pipelineId', v)
                      set('inputNode', '')
                    }}
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue placeholder={t('dialog.pipelinePlaceholder')} />
                    </SelectTrigger>
                    <SelectContent>
                      {(pipelines ?? []).map((p) => (
                        <SelectItem key={p.id} value={p.id}>
                          {p.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {pipelines !== null && pipelines.length === 0 ? (
                    <p className="text-[11px] text-muted-foreground">
                      {t('dialog.pipelineNone')}
                    </p>
                  ) : null}
                </div>
                <div className="space-y-1.5">
                  <label className="text-sm font-medium">
                    {t('dialog.inputNode')}
                  </label>
                  {inputNodes === null && form.pipelineId ? (
                    <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                      {t('dialog.pipelineLoading')}
                    </p>
                  ) : (
                    <Select
                      value={form.inputNode || undefined}
                      onValueChange={(v) => set('inputNode', v)}
                      disabled={!form.pipelineId}
                    >
                      <SelectTrigger className="w-full">
                        <SelectValue
                          placeholder={t('dialog.inputNodePlaceholder')}
                        />
                      </SelectTrigger>
                      <SelectContent>
                        {inputNodes !== null &&
                          inputNodes.map((n) => (
                            <SelectItem key={n.id} value={n.id}>
                              {n.label && n.label !== n.id
                                ? `${n.label} (${n.id})`
                                : n.id}
                            </SelectItem>
                          ))}
                      </SelectContent>
                    </Select>
                  )}
                  {inputNodes !== null && inputNodes.length === 0 && form.pipelineId ? (
                    <p className="text-[11px] text-muted-foreground">
                      {t('dialog.inputNodeNone')}
                    </p>
                  ) : (
                    <p className="text-[11px] text-muted-foreground">
                      {t('dialog.inputNodeHint')}
                    </p>
                  )}
                </div>
              </div>
            ) : null}
          </section>

          {/* ── 输出配置（仅直接模式；§5.3 校验链第 6 条 direct 必填 output） ── */}
          {form.mode !== 'pipeline' ? (
            <section className="space-y-3">
              <h3 className="text-sm font-semibold">{t('dialog.section.output')}</h3>
              <p className="text-[11px] text-muted-foreground">
                {t('dialog.outputHint')}
              </p>
              <div className="space-y-1.5">
                <label className="text-sm font-medium">{t('dialog.destDir')}</label>
                <Input
                  value={form.dest_dir}
                  onChange={(e) => set('dest_dir', e.target.value)}
                  placeholder={t('dialog.destDirPlaceholder')}
                  className="font-mono text-xs"
                />
              </div>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="space-y-1.5">
                  <label className="text-sm font-medium">
                    {t('dialog.nameTemplate')}
                  </label>
                  <Input
                    value={form.name_template}
                    onChange={(e) => set('name_template', e.target.value)}
                    placeholder={t('dialog.nameTemplatePlaceholder')}
                    className="font-mono text-xs"
                  />
                  <p className="text-[11px] text-muted-foreground">
                    {t('dialog.nameTemplateHint')}
                  </p>
                </div>
                <div className="space-y-1.5">
                  <label className="text-sm font-medium">
                    {t('dialog.onConflict')}
                  </label>
                  <Select
                    value={form.on_conflict}
                    onValueChange={(v) => set('on_conflict', v as WatcherConflictPolicy)}
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="suffix">{t('conflict.suffix')}</SelectItem>
                      <SelectItem value="overwrite">
                        {t('conflict.overwrite')}
                      </SelectItem>
                      <SelectItem value="skip">{t('conflict.skip')}</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              </div>
            </section>
          ) : null}
        </div>
        <DialogFooter className="gap-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => onOpenChange(false)}
            disabled={saving}
          >
            {i18n.t('common:action.cancel')}
          </Button>
          <Button size="sm" onClick={() => void handleSave()} disabled={saving}>
            {saving ? (
              <>
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                {t('dialog.saving')}
              </>
            ) : (
              t('dialog.save')
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ── 规则详情触发历史（统一事件日志 §5.7：GET /api/events?rule=<id>） ─────

function HistoryDialog({
  rule,
  onOpenChange,
}: {
  rule: WatchRule | null
  onOpenChange: (open: boolean) => void
}) {
  const { t, i18n } = useTranslation('triggers')
  const [events, setEvents] = useState<UnifiedEvent[] | null>(null)
  const [loading, setLoading] = useState(false)

  const load = useCallback((ruleId: string) => {
    setLoading(true)
    api
      .events({ rule: ruleId, limit: HISTORY_LIMIT })
      .then((resp) => setEvents(resp.events ?? []))
      .catch(() => {
        setEvents([])
        toast.error(t('history.loadFailed'))
      })
      .finally(() => setLoading(false))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t])

  useEffect(() => {
    if (rule) load(rule.id)
    else setEvents(null)
  }, [rule, load])

  return (
    <Dialog
      open={rule !== null}
      onOpenChange={(v) => {
        if (!v) onOpenChange(false)
      }}
    >
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>
            {t('history.title')}
            {rule ? (
              <span className="ml-2 font-mono text-xs text-muted-foreground">
                {rule.name}
              </span>
            ) : null}
          </DialogTitle>
          <DialogDescription className="sr-only">
            {t('history.title')}
          </DialogDescription>
        </DialogHeader>
        <div className="max-h-[60vh] overflow-y-auto">
          {loading && events === null ? (
            <div className="flex items-center justify-center py-10 text-muted-foreground">
              <Loader2 className="h-5 w-5 animate-spin" />
            </div>
          ) : events !== null && events.length === 0 ? (
            <p className="py-10 text-center text-sm text-muted-foreground">
              {t('history.empty')}
            </p>
          ) : events !== null ? (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="w-40">{t('history.col.time')}</TableHead>
                  <TableHead>{t('history.col.file')}</TableHead>
                  <TableHead className="w-28">{t('history.col.status')}</TableHead>
                  <TableHead className="w-44">{t('history.col.taskId')}</TableHead>
                  <TableHead>{t('history.col.detail')}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {events.map((e, idx) => (
                  <TableRow key={idx}>
                    <TableCell className="whitespace-nowrap text-xs">
                      {formatTs(e.ts, i18n.language)}
                    </TableCell>
                    <TableCell className="max-w-56">
                      <span className="block truncate font-mono text-xs" title={e.file}>
                        {e.file ? basename(e.file) : (e.rule ?? '—')}
                      </span>
                    </TableCell>
                    <TableCell>
                      <span className="flex items-center gap-1.5 text-xs">
                        <span
                          className={cn(
                            'h-1.5 w-1.5 shrink-0 rounded-full',
                            statusDotClass(e.status),
                          )}
                        />
                        {e.status
                          ? t(`status.${e.status}`, { defaultValue: e.status })
                          : '—'}
                      </span>
                    </TableCell>
                    <TableCell className="max-w-44">
                      <span
                        className="block truncate font-mono text-[11px] text-muted-foreground"
                        title={e.task_id}
                      >
                        {e.task_id ?? '—'}
                      </span>
                    </TableCell>
                    <TableCell className="max-w-48">
                      <span
                        className="block truncate text-xs text-muted-foreground"
                        title={e.detail ?? e.error}
                      >
                        {e.detail ?? e.error ?? '—'}
                      </span>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          ) : null}
        </div>
        <DialogFooter className="gap-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => rule && load(rule.id)}
            disabled={loading || !rule}
          >
            <RefreshCw className={cn('h-3.5 w-3.5', loading && 'animate-spin')} />
            {t('history.refresh')}
          </Button>
          <Button size="sm" variant="outline" onClick={() => onOpenChange(false)}>
            {i18n.t('common:action.close')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ── 统一事件历史（T-27：GET /api/events 聚合展示；preset 给定时按钩子/规则过滤）──

type EventTypeFilter = '' | 'watcher_trigger' | 'task_terminal' | 'webhook_trigger'

function EventsDialog({
  preset,
  presetLabel,
  onOpenChange,
}: {
  /** 给定时不显示类型筛选（按 rule+type 精确过滤） */
  preset?: { rule?: string; type?: string } | null
  presetLabel?: string
  onOpenChange: (open: boolean) => void
}) {
  const { t, i18n } = useTranslation('triggers')
  const [typeFilter, setTypeFilter] = useState<EventTypeFilter>('')
  const [events, setEvents] = useState<UnifiedEvent[] | null>(null)
  const [loading, setLoading] = useState(false)

  const query = preset ?? { type: typeFilter || undefined }

  const load = useCallback(() => {
    setLoading(true)
    api
      .events({ ...query, limit: 200 })
      .then((resp) => setEvents(resp.events ?? []))
      .catch(() => {
        setEvents([])
        toast.error(t('events.loadFailed'))
      })
      .finally(() => setLoading(false))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, query.rule, query.type])

  useEffect(() => {
    load()
  }, [load])

  return (
    <Dialog
      open
      onOpenChange={(v) => {
        if (!v) onOpenChange(false)
      }}
    >
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>
            {t('events.title')}
            {presetLabel ? (
              <span className="ml-2 font-mono text-xs text-muted-foreground">
                {presetLabel}
              </span>
            ) : null}
          </DialogTitle>
          <DialogDescription className="sr-only">{t('events.title')}</DialogDescription>
        </DialogHeader>
        {!preset ? (
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground">{t('events.filter.label')}</span>
            <Select
              value={typeFilter || 'all'}
              onValueChange={(v) => setTypeFilter(v === 'all' ? '' : (v as EventTypeFilter))}
            >
              <SelectTrigger className="w-48">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t('events.filter.all')}</SelectItem>
                <SelectItem value="watcher_trigger">{t('events.filter.watcher')}</SelectItem>
                <SelectItem value="task_terminal">{t('events.filter.task')}</SelectItem>
                <SelectItem value="webhook_trigger">{t('events.filter.webhook')}</SelectItem>
              </SelectContent>
            </Select>
          </div>
        ) : null}
        <div className="max-h-[55vh] overflow-y-auto">
          {loading && events === null ? (
            <div className="flex items-center justify-center py-10 text-muted-foreground">
              <Loader2 className="h-5 w-5 animate-spin" />
            </div>
          ) : events !== null && events.length === 0 ? (
            <p className="py-10 text-center text-sm text-muted-foreground">
              {t('events.empty')}
            </p>
          ) : events !== null ? (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="w-40">{t('events.col.time')}</TableHead>
                  <TableHead className="w-32">{t('events.col.type')}</TableHead>
                  <TableHead>{t('events.col.source')}</TableHead>
                  <TableHead className="w-24">{t('events.col.status')}</TableHead>
                  <TableHead className="w-40">{t('events.col.task')}</TableHead>
                  <TableHead>{t('events.col.detail')}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {events.map((e, idx) => (
                  <TableRow key={idx}>
                    <TableCell className="whitespace-nowrap text-xs">
                      {formatTs(e.ts, i18n.language)}
                    </TableCell>
                    <TableCell className="text-xs">{t(`events.type.${e.type}`, { defaultValue: e.type })}</TableCell>
                    <TableCell className="max-w-40">
                      <span
                        className="block truncate font-mono text-xs text-muted-foreground"
                        title={e.rule ?? e.pipeline_id}
                      >
                        {e.rule ?? e.pipeline_id ?? '—'}
                      </span>
                    </TableCell>
                    <TableCell>
                      <span className="flex items-center gap-1.5 text-xs">
                        <span
                          className={cn('h-1.5 w-1.5 shrink-0 rounded-full', statusDotClass(e.status))}
                        />
                        {e.status
                          ? t(`status.${e.status}`, { defaultValue: e.status })
                          : '—'}
                      </span>
                    </TableCell>
                    <TableCell className="max-w-40">
                      <span
                        className="block truncate font-mono text-[11px] text-muted-foreground"
                        title={e.task_id}
                      >
                        {e.task_id ?? '—'}
                      </span>
                    </TableCell>
                    <TableCell className="max-w-48">
                      <span
                        className="block truncate text-xs text-muted-foreground"
                        title={e.detail ?? e.file ?? e.error}
                      >
                        {e.detail ?? e.file ?? e.error ?? '—'}
                      </span>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          ) : null}
        </div>
        <DialogFooter className="gap-2">
          <Button variant="ghost" size="sm" onClick={load} disabled={loading}>
            <RefreshCw className={cn('h-3.5 w-3.5', loading && 'animate-spin')} />
            {t('events.refresh')}
          </Button>
          <Button size="sm" variant="outline" onClick={() => onOpenChange(false)}>
            {i18n.t('common:action.close')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ── 定时计划（T-27 cron 双入口：本面板与 pipeline.tsx ScheduleDialog 共用
//    同一 PUT/DELETE /pipelines/{id}/schedule API，物理迁移不做）────────────

function CronDialog({
  open,
  onOpenChange,
  editing,
  onSaved,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** 编辑目标（pipeline_id 固定）；null = 新建 */
  editing: ScheduleListEntry | null
  onSaved: () => void
}) {
  const { t } = useTranslation('triggers')
  const [pipelineId, setPipelineId] = useState('')
  const [cron, setCron] = useState('0 3 * * *')
  const [enabled, setEnabled] = useState(true)
  const [pipelines, setPipelines] = useState<PipelineSummary[] | null>(null)
  const [saving, setSaving] = useState(false)

  useEffect(() => {
    if (!open) return
    setPipelineId(editing?.pipeline_id ?? '')
    setCron(editing?.cron ?? '0 3 * * *')
    setEnabled(editing?.enabled ?? true)
    setPipelines(null)
    api
      .listPipelines()
      .then(setPipelines)
      .catch(() => setPipelines([]))
  }, [open, editing])

  const handleSave = useCallback(async () => {
    if (!pipelineId.trim()) {
      toast.error(t('sched.validation.pipelineRequired'))
      return
    }
    if (!cron.trim()) {
      toast.error(t('sched.validation.cronRequired'))
      return
    }
    setSaving(true)
    try {
      await api.putSchedule(pipelineId, { cron: cron.trim(), enabled })
      toast.success(t('sched.saved'))
      onOpenChange(false)
      onSaved()
    } catch (err) {
      toast.error(t('sched.saveFailed'), { description: displayBackendError(err, t) })
    } finally {
      setSaving(false)
    }
  }, [pipelineId, cron, enabled, t, onOpenChange, onSaved])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{editing ? t('sched.edit') : t('sched.create')}</DialogTitle>
          <DialogDescription className="sr-only">{t('sched.create')}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-1">
          <div className="space-y-1.5">
            <label className="text-sm font-medium">{t('sched.pipeline')}</label>
            <Select
              value={pipelineId || undefined}
              onValueChange={setPipelineId}
              disabled={editing !== null}
            >
              <SelectTrigger className="w-full">
                <SelectValue placeholder={t('sched.pipelinePlaceholder')} />
              </SelectTrigger>
              <SelectContent>
                {(pipelines ?? []).map((p) => (
                  <SelectItem key={p.id} value={p.id}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {pipelines !== null && pipelines.length === 0 ? (
              <p className="text-[11px] text-muted-foreground">{t('sched.pipelineNone')}</p>
            ) : (
              <p className="text-[11px] text-muted-foreground">{t('sched.pipelineHint')}</p>
            )}
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium">{t('sched.cron')}</label>
            <Input
              value={cron}
              onChange={(e) => setCron(e.target.value)}
              placeholder="0 3 * * *"
              className="font-mono text-xs"
            />
            <p className="text-[11px] text-muted-foreground">{t('sched.cronHint')}</p>
          </div>
          <div className="flex items-center justify-between rounded-lg border border-border px-3 py-2.5">
            <p className="text-sm">{t('sched.enabled')}</p>
            <Switch checked={enabled} onCheckedChange={setEnabled} aria-label={t('sched.enabled')} />
          </div>
        </div>
        <DialogFooter className="gap-2">
          <Button variant="ghost" size="sm" onClick={() => onOpenChange(false)} disabled={saving}>
            {t('common:action.cancel')}
          </Button>
          <Button size="sm" onClick={() => void handleSave()} disabled={saving}>
            {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
            {t('dialog.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function SchedulesPanel({ refreshSignal = 0 }: { refreshSignal?: number }) {
  const { t } = useTranslation('triggers')
  const [list, setList] = useState<ScheduleListEntry[] | null>(null)
  const [reloadKey, setReloadKey] = useState(0)
  const [editorOpen, setEditorOpen] = useState(false)
  const [editing, setEditing] = useState<ScheduleListEntry | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<ScheduleListEntry | null>(null)
  const [deleting, setDeleting] = useState(false)

  const reload = useCallback(() => setReloadKey((k) => k + 1), [])

  useEffect(() => {
    let cancelled = false
    setList(null)
    api
      .listSchedules()
      .then((resp) => !cancelled && setList(resp.schedules ?? []))
      .catch(() => !cancelled && setList([]))
    return () => {
      cancelled = true
    }
  }, [reloadKey, refreshSignal])

  const handleToggle = async (entry: ScheduleListEntry, enabled: boolean) => {
    try {
      await api.putSchedule(entry.pipeline_id, { cron: entry.cron, enabled })
      reload()
    } catch (err) {
      toast.error(t('sched.saveFailed'), { description: displayBackendError(err, t) })
    }
  }

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await api.deleteSchedule(deleteTarget.pipeline_id)
      toast.success(t('sched.deleted'))
      setDeleteTarget(null)
      reload()
    } catch (err) {
      toast.error(t('sched.deleteFailed'), { description: displayBackendError(err, t) })
    } finally {
      setDeleting(false)
    }
  }

  if (list === null) {
    return (
      <div className="space-y-2">
        {Array.from({ length: 3 }).map((_, i) => (
          <Skeleton key={i} className="h-14 w-full" />
        ))}
      </div>
    )
  }
  if (list.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
        <Clock className="h-10 w-10 text-muted-foreground/50" />
        <div>
          <p className="font-medium">{t('sched.empty')}</p>
          <p className="mt-1 text-sm text-muted-foreground">{t('sched.emptyDescription')}</p>
        </div>
        <Button
          size="sm"
          onClick={() => {
            setEditing(null)
            setEditorOpen(true)
          }}
        >
          <Plus className="h-4 w-4" />
          {t('sched.create')}
        </Button>
      </div>
    )
  }
  return (
    <>
      <div className="mb-3 flex justify-end">
        <Button
          size="sm"
          onClick={() => {
            setEditing(null)
            setEditorOpen(true)
          }}
        >
          <Plus className="h-4 w-4" />
          {t('sched.create')}
        </Button>
      </div>
      <div className="overflow-x-auto rounded-lg border border-border">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('sched.col.pipeline')}</TableHead>
              <TableHead>{t('sched.col.cron')}</TableHead>
              <TableHead className="w-20">{t('table.enabled')}</TableHead>
              <TableHead>{t('sched.col.lastTask')}</TableHead>
              <TableHead className="w-24 text-right">{t('table.actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {list.map((entry) => (
              <TableRow key={entry.pipeline_id}>
                <TableCell className="font-medium">{entry.pipeline_id}</TableCell>
                <TableCell>
                  <span className="font-mono text-xs text-muted-foreground">{entry.cron}</span>
                </TableCell>
                <TableCell>
                  <Switch
                    checked={entry.enabled}
                    onCheckedChange={(v) => void handleToggle(entry, v)}
                    aria-label={`${t('table.enabled')}: ${entry.pipeline_id}`}
                  />
                </TableCell>
                <TableCell className="max-w-44">
                  <span
                    className="block truncate font-mono text-[11px] text-muted-foreground"
                    title={entry.last_task_id}
                  >
                    {entry.last_task_id ?? '—'}
                  </span>
                </TableCell>
                <TableCell className="text-right">
                  <div className="flex items-center justify-end gap-1">
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => {
                        setEditing(entry)
                        setEditorOpen(true)
                      }}
                      aria-label={t('action.edit')}
                      title={t('action.edit')}
                    >
                      <Pencil className="h-4 w-4" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setDeleteTarget(entry)}
                      aria-label={t('action.delete')}
                      title={t('action.delete')}
                      className="text-destructive hover:text-destructive"
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>

      <CronDialog
        open={editorOpen}
        onOpenChange={setEditorOpen}
        editing={editing}
        onSaved={reload}
      />

      <Dialog
        open={deleteTarget !== null}
        onOpenChange={(v) => {
          if (!v) setDeleteTarget(null)
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t('sched.deleteTitle')}</DialogTitle>
            <DialogDescription>
              {t('sched.deleteMessage', { id: deleteTarget?.pipeline_id ?? '' })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="gap-2">
            <Button variant="ghost" size="sm" onClick={() => setDeleteTarget(null)} disabled={deleting}>
              {t('common:action.cancel')}
            </Button>
            <Button size="sm" variant="destructive" onClick={() => void handleDelete()} disabled={deleting}>
              {deleting ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
              {t('action.delete')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}

// ── Webhook 触发器（T-27：hook_id 触发入口；鉴权由 /api 全局访问保护覆盖）──

/** 列表/对话框共用的目标摘要（含模块能力 / 管线 id 附加信息） */
function hookTargetSummary(target: WebhookTargetInput, t: TFunction): string {
  if (target.type === 'pipeline') {
    return `${t('mode.pipeline')} · ${target.pipeline_id}`
  }
  return `${t('hook.direct')} · ${target.module_id}/${target.capability}`
}

interface HookForm {
  name: string
  enabled: boolean
  mode: 'pipeline' | 'direct'
  pipelineId: string
  moduleId: string
  capability: string
  input_path: string
}

const DEFAULT_HOOK_FORM: HookForm = {
  name: '',
  enabled: true,
  mode: 'pipeline',
  pipelineId: '',
  moduleId: '',
  capability: '',
  input_path: '',
}

function HookDialog({
  open,
  onOpenChange,
  hook,
  onSaved,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** 编辑目标；null = 新建 */
  hook: WebhookEntry | null
  onSaved: () => void
}) {
  const { t } = useTranslation('triggers')
  const [form, setForm] = useState<HookForm>(DEFAULT_HOOK_FORM)
  const [saving, setSaving] = useState(false)
  const [pipelines, setPipelines] = useState<PipelineSummary[] | null>(null)
  const [modules, setModules] = useState<ModuleResponse[] | null>(null)
  const [browserOpen, setBrowserOpen] = useState(false)

  const set = <K extends keyof HookForm>(key: K, value: HookForm[K]) =>
    setForm((prev) => ({ ...prev, [key]: value }))

  useEffect(() => {
    if (!open) return
    if (hook) {
      setForm({
        name: hook.name,
        enabled: hook.enabled,
        mode: hook.target.type,
        pipelineId: hook.target.type === 'pipeline' ? hook.target.pipeline_id : '',
        moduleId: hook.target.type === 'direct' ? hook.target.module_id : '',
        capability: hook.target.type === 'direct' ? hook.target.capability : '',
        input_path: hook.target.type === 'direct' ? hook.target.input_path : '',
      })
    } else {
      setForm(DEFAULT_HOOK_FORM)
    }
    setPipelines(null)
    setModules(null)
    api
      .listPipelines()
      .then(setPipelines)
      .catch(() => setPipelines([]))
    api
      .modules()
      .then((list) => setModules(list.filter((m) => (m.capabilities ?? []).length > 0)))
      .catch(() => setModules([]))
  }, [open, hook])

  const selectedModule = useMemo(
    () => modules?.find((m) => m.id === form.moduleId) ?? null,
    [modules, form.moduleId],
  )

  const formToInput = (): { name: string; enabled: boolean; target: WebhookTargetInput } => {
    const target: WebhookTargetInput =
      form.mode === 'pipeline'
        ? { type: 'pipeline', pipeline_id: form.pipelineId, inputs: {}, params: {} }
        : {
            type: 'direct',
            module_id: form.moduleId,
            capability: form.capability,
            input_path: form.input_path.trim(),
            params: {},
          }
    return { name: form.name.trim(), enabled: form.enabled, target }
  }

  const handleSave = useCallback(async () => {
    if (!form.name.trim()) {
      toast.error(t('hook.validation.nameRequired'))
      return
    }
    if (form.mode === 'pipeline' && !form.pipelineId) {
      toast.error(t('hook.validation.pipelineRequired'))
      return
    }
    if (form.mode === 'direct') {
      if (!form.moduleId || !form.capability) {
        toast.error(t('hook.validation.capabilityRequired'))
        return
      }
      if (!form.input_path.trim()) {
        toast.error(t('hook.validation.inputPathRequired'))
        return
      }
    }
    setSaving(true)
    try {
      const body = formToInput()
      if (hook) {
        await api.updateHook(hook.id, body)
      } else {
        await api.createHook(body)
      }
      toast.success(t('hook.saved'))
      onOpenChange(false)
      onSaved()
    } catch (err) {
      toast.error(t('hook.saveFailed'), { description: displayBackendError(err, t) })
    } finally {
      setSaving(false)
    }
    // formToInput 读取同一 form 闭包
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form, hook, t, onOpenChange, onSaved])

  const modeItems = [
    { value: 'pipeline' as const, label: t('hook.mode.pipeline') },
    { value: 'direct' as const, label: t('hook.mode.direct') },
  ]

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {hook ? t('hook.editTitle') : t('hook.createTitle')}
            {hook ? (
              <span className="ml-2 font-mono text-xs text-muted-foreground">{hook.id}</span>
            ) : null}
          </DialogTitle>
          <DialogDescription className="sr-only">{t('hook.createTitle')}</DialogDescription>
        </DialogHeader>
        <div className="max-h-[70vh] space-y-4 overflow-y-auto py-2 pr-1">
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="space-y-1.5">
              <label className="text-sm font-medium">{t('hook.name')}</label>
              <Input
                value={form.name}
                onChange={(e) => set('name', e.target.value)}
                placeholder={t('hook.namePlaceholder')}
              />
            </div>
            <div className="flex items-end justify-between rounded-lg border border-border px-3 py-2.5">
              <p className="text-sm">{t('hook.enabled')}</p>
              <Switch checked={form.enabled} onCheckedChange={(v) => set('enabled', v)} aria-label={t('hook.enabled')} />
            </div>
          </div>

          <div className="space-y-1.5">
            <label className="text-sm font-medium">{t('hook.targetMode')}</label>
            <SegmentedTabs
              items={modeItems}
              value={form.mode}
              onChange={(v) => set('mode', v)}
              ariaLabel={t('hook.targetMode')}
              className="w-fit"
            />
            <p className="text-[11px] text-muted-foreground">
              {form.mode === 'pipeline' ? t('hook.mode.pipelineDesc') : t('hook.mode.directDesc')}
            </p>
          </div>

          {form.mode === 'pipeline' ? (
            <div className="space-y-1.5">
              <label className="text-sm font-medium">{t('sched.pipeline')}</label>
              <Select value={form.pipelineId || undefined} onValueChange={(v) => set('pipelineId', v)}>
                <SelectTrigger className="w-full">
                  <SelectValue placeholder={t('sched.pipelinePlaceholder')} />
                </SelectTrigger>
                <SelectContent>
                  {(pipelines ?? []).map((p) => (
                    <SelectItem key={p.id} value={p.id}>
                      {p.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-[11px] text-muted-foreground">{t('hook.pipelineHint')}</p>
            </div>
          ) : (
            <div className="space-y-3">
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="space-y-1.5">
                  <label className="text-sm font-medium">{t('dialog.module')}</label>
                  <Select
                    value={form.moduleId || undefined}
                    onValueChange={(v) => {
                      set('moduleId', v)
                      set('capability', '')
                    }}
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue placeholder={t('dialog.modulePlaceholder')} />
                    </SelectTrigger>
                    <SelectContent>
                      {(modules ?? []).map((m) => (
                        <SelectItem key={m.id} value={m.id}>
                          {m.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <div className="space-y-1.5">
                  <label className="text-sm font-medium">{t('dialog.capability')}</label>
                  <Select
                    value={form.capability || undefined}
                    onValueChange={(v) => set('capability', v)}
                    disabled={!form.moduleId}
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue placeholder={t('dialog.capabilityPlaceholder')} />
                    </SelectTrigger>
                    <SelectContent>
                      {(selectedModule?.capabilities ?? []).map((cap) => (
                        <SelectItem key={cap.name} value={cap.name}>
                          {cap.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              </div>
              <div className="space-y-1.5">
                <label className="text-sm font-medium">{t('hook.inputPath')}</label>
                <Input
                  value={form.input_path}
                  onChange={(e) => set('input_path', e.target.value)}
                  placeholder={t('hook.inputPathPlaceholder')}
                  className="font-mono text-xs"
                />
                <div className="flex items-center justify-between gap-2">
                  <p className="text-[11px] text-muted-foreground">{t('hook.inputPathHint')}</p>
                  <Button variant="outline" size="sm" onClick={() => setBrowserOpen(true)}>
                    {t('storeBrowser.browse', { ns: 'pipeline' })}
                  </Button>
                </div>
              </div>
            </div>
          )}
        </div>
        <DialogFooter className="gap-2">
          <Button variant="ghost" size="sm" onClick={() => onOpenChange(false)} disabled={saving}>
            {t('common:action.cancel')}
          </Button>
          <Button size="sm" onClick={() => void handleSave()} disabled={saving}>
            {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
            {t('dialog.save')}
          </Button>
        </DialogFooter>

        <StoreBrowserDialog
          open={browserOpen}
          onOpenChange={setBrowserOpen}
          onPicked={(storeRef) => set('input_path', storeRef)}
        />
      </DialogContent>
    </Dialog>
  )
}

function WebhooksPanel({ refreshSignal = 0 }: { refreshSignal?: number }) {
  const { t } = useTranslation('triggers')
  const [list, setList] = useState<WebhookEntry[] | null>(null)
  const [reloadKey, setReloadKey] = useState(0)
  const [editorOpen, setEditorOpen] = useState(false)
  const [editing, setEditing] = useState<WebhookEntry | null>(null)
  const [historyHook, setHistoryHook] = useState<WebhookEntry | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<WebhookEntry | null>(null)
  const [deleting, setDeleting] = useState(false)
  const [triggering, setTriggering] = useState<string | null>(null)

  const reload = useCallback(() => setReloadKey((k) => k + 1), [])

  useEffect(() => {
    let cancelled = false
    setList(null)
    api
      .listHooks()
      .then((resp) => !cancelled && setList(resp))
      .catch(() => !cancelled && setList([]))
    return () => {
      cancelled = true
    }
  }, [reloadKey, refreshSignal])

  const handleToggle = async (hook: WebhookEntry, enabled: boolean) => {
    try {
      await api.updateHook(hook.id, { name: hook.name, enabled, target: hook.target })
      reload()
    } catch (err) {
      toast.error(t('hook.saveFailed'), { description: displayBackendError(err, t) })
    }
  }

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await api.deleteHook(deleteTarget.id)
      toast.success(t('hook.deleted'))
      setDeleteTarget(null)
      reload()
    } catch (err) {
      toast.error(t('hook.deleteFailed'), { description: displayBackendError(err, t) })
    } finally {
      setDeleting(false)
    }
  }

  const handleTrigger = async (hook: WebhookEntry) => {
    setTriggering(hook.id)
    try {
      const resp = await api.triggerHook(hook.id)
      toast.success(t('hook.triggered'), { description: resp.task_id })
      reload()
    } catch (err) {
      toast.error(t('hook.triggerFailed'), { description: displayBackendError(err, t) })
    } finally {
      setTriggering(null)
    }
  }

  if (list === null) {
    return (
      <div className="space-y-2">
        {Array.from({ length: 3 }).map((_, i) => (
          <Skeleton key={i} className="h-14 w-full" />
        ))}
      </div>
    )
  }
  if (list.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
        <Webhook className="h-10 w-10 text-muted-foreground/50" />
        <div>
          <p className="font-medium">{t('hook.empty')}</p>
          <p className="mt-1 text-sm text-muted-foreground">{t('hook.emptyDescription')}</p>
        </div>
        <Button
          size="sm"
          onClick={() => {
            setEditing(null)
            setEditorOpen(true)
          }}
        >
          <Plus className="h-4 w-4" />
          {t('hook.createTitle')}
        </Button>
      </div>
    )
  }
  return (
    <>
      <div className="mb-3 flex justify-end">
        <Button
          size="sm"
          onClick={() => {
            setEditing(null)
            setEditorOpen(true)
          }}
        >
          <Plus className="h-4 w-4" />
          {t('hook.createTitle')}
        </Button>
      </div>
      <div className="overflow-x-auto rounded-lg border border-border">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('table.name')}</TableHead>
              <TableHead>{t('hook.col.hookId')}</TableHead>
              <TableHead>{t('table.action')}</TableHead>
              <TableHead className="w-20">{t('table.enabled')}</TableHead>
              <TableHead className="w-52 text-right">{t('table.actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {list.map((hook) => (
              <TableRow key={hook.id}>
                <TableCell>
                  <span className="font-medium">{hook.name}</span>
                </TableCell>
                <TableCell>
                  <button
                    type="button"
                    onClick={() => void copyHookUrl(hook.id, t)}
                    className="flex items-center gap-1 font-mono text-xs text-muted-foreground underline-offset-2 hover:underline"
                    title={t('hook.copyHint')}
                  >
                    <Copy className="h-3 w-3" />
                    {hook.id}
                  </button>
                </TableCell>
                <TableCell>
                  <Badge variant="outline" className="max-w-64 font-normal">
                    <span className="truncate">{hookTargetSummary(hook.target, t)}</span>
                  </Badge>
                </TableCell>
                <TableCell>
                  <Switch
                    checked={hook.enabled}
                    onCheckedChange={(v) => void handleToggle(hook, v)}
                    aria-label={`${t('table.enabled')}: ${hook.name}`}
                  />
                </TableCell>
                <TableCell className="text-right">
                  <div className="flex items-center justify-end gap-1">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => void handleTrigger(hook)}
                      disabled={!hook.enabled || triggering === hook.id}
                      title={t('hook.trigger')}
                    >
                      {triggering === hook.id ? (
                        <Loader2 className="h-3.5 w-3.5 animate-spin" />
                      ) : null}
                      {t('hook.trigger')}
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setHistoryHook(hook)}
                      aria-label={t('action.history')}
                      title={t('action.history')}
                    >
                      <History className="h-4 w-4" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => {
                        setEditing(hook)
                        setEditorOpen(true)
                      }}
                      aria-label={t('action.edit')}
                      title={t('action.edit')}
                    >
                      <Pencil className="h-4 w-4" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setDeleteTarget(hook)}
                      aria-label={t('action.delete')}
                      title={t('action.delete')}
                      className="text-destructive hover:text-destructive"
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>

      <HookDialog
        open={editorOpen}
        onOpenChange={setEditorOpen}
        hook={editing}
        onSaved={reload}
      />
      {historyHook ? (
        <EventsDialog
          preset={{ rule: historyHook.id, type: 'webhook_trigger' }}
          presetLabel={historyHook.name}
          onOpenChange={() => setHistoryHook(null)}
        />
      ) : null}

      <Dialog
        open={deleteTarget !== null}
        onOpenChange={(v) => {
          if (!v) setDeleteTarget(null)
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t('hook.deleteTitle')}</DialogTitle>
            <DialogDescription>
              {t('hook.deleteMessage', { name: deleteTarget?.name ?? '' })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="gap-2">
            <Button variant="ghost" size="sm" onClick={() => setDeleteTarget(null)} disabled={deleting}>
              {t('common:action.cancel')}
            </Button>
            <Button size="sm" variant="destructive" onClick={() => void handleDelete()} disabled={deleting}>
              {deleting ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
              {t('action.delete')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}

/** 复制 hook 触发 URL（模块级函数避免在行内反复绑定） */
async function copyHookUrl(id: string, t: TFunction): Promise<void> {
  const url = `${window.location.origin}/api/hooks/${id}`
  try {
    await navigator.clipboard.writeText(url)
    toast.success(t('hook.copied'), { description: url })
  } catch {
    toast.error(t('hook.copyFailed'), { description: url })
  }
}

// ── 触发器页 ────────────────────────────────────────────────────────────

/** 触发器页 tab（T-27 三型入口：目录监控 / 定时 cron / webhook） */
type TriggersTab = 'watchers' | 'schedules' | 'webhooks'

export function TriggersPage() {
  const { t } = useTranslation('triggers')
  const [tab, setTab] = useState<TriggersTab>('watchers')
  const [rules, setRules] = useState<WatchRule[] | null>(null)
  const [reloadKey, setReloadKey] = useState(0)
  const [editorOpen, setEditorOpen] = useState(false)
  const [editing, setEditing] = useState<WatchRule | null>(null)
  const [historyRule, setHistoryRule] = useState<WatchRule | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<WatchRule | null>(null)
  const [deleting, setDeleting] = useState(false)
  // T-27：面板刷新信号（schedules/webhooks 面板各自消费）与统一事件历史
  const [panelRefresh, setPanelRefresh] = useState(0)
  const [eventsOpen, setEventsOpen] = useState(false)

  const reload = useCallback(() => setReloadKey((k) => k + 1), [])
  const refreshCurrent = useCallback(() => {
    setReloadKey((k) => k + 1)
    setPanelRefresh((k) => k + 1)
  }, [])

  useEffect(() => {
    if (tab !== 'watchers') return
    let cancelled = false
    setRules(null)
    api
      .listWatchers()
      .then((list) => !cancelled && setRules(list))
      .catch(() => !cancelled && setRules([]))
    return () => {
      cancelled = true
    }
  }, [reloadKey, tab])

  const openCreate = () => {
    setEditing(null)
    setEditorOpen(true)
  }

  const openEdit = (rule: WatchRule) => {
    setEditing(rule)
    setEditorOpen(true)
  }

  const handleToggle = async (rule: WatchRule, enabled: boolean) => {
    // PUT 全量更新：以当前规则内容重建请求体，仅翻转 enabled
    const form: RuleForm = {
      name: rule.name,
      watch_dir: rule.watch_dir,
      extensionsText: (rule.extensions ?? []).join(', '),
      excludeText: (rule.exclude ?? []).join(', '),
      recursive: rule.recursive ?? false,
      include_modified: rule.include_modified ?? false,
      stability_secs: String(rule.stability_secs ?? 30),
      backfill: rule.backfill ?? false,
      dedup_enabled: rule.dedup?.enabled ?? true,
      dedup_max_mb: String(rule.dedup?.max_hash_mb ?? 64),
      enabled,
      mode: ruleMode(rule),
      moduleId:
        (rule.direct?.kind?.type ?? '').toLowerCase() === 'module'
          ? ((rule.direct?.kind as { module_id?: string }).module_id ?? '')
          : '',
      capability:
        (rule.direct?.kind?.type ?? '').toLowerCase() === 'module'
          ? ((rule.direct?.kind as { capability?: string }).capability ?? '')
          : '',
      pipelineId: rule.pipeline?.pipeline_id ?? '',
      inputNode: rule.pipeline?.input_node ?? '',
      dest_dir: rule.output?.dest_dir ?? '',
      name_template: rule.output?.name_template ?? '{name}.{ext}',
      on_conflict: rule.output?.on_conflict ?? 'suffix',
    }
    try {
      await api.updateWatcher(rule.id, formToInput(form))
      reload()
    } catch (err) {
      toast.error(t('toggle.failed'), {
        description: displayBackendError(err, t),
      })
    }
  }

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await api.deleteWatcher(deleteTarget.id)
      toast.success(t('delete.success'))
      setDeleteTarget(null)
      reload()
    } catch (err) {
      toast.error(t('delete.failed'), {
        description: displayBackendError(err, t),
      })
    } finally {
      setDeleting(false)
    }
  }

  const tabItems = [
    { value: 'watchers' as TriggersTab, label: t('tabs.watchers') },
    { value: 'schedules' as TriggersTab, label: t('tabs.schedules') },
    { value: 'webhooks' as TriggersTab, label: t('tabs.webhooks') },
  ]

  return (
    <PageContainer
      title={t('title')}
      description={t('description')}
      actions={
        <>
          <Button variant="outline" size="sm" onClick={refreshCurrent}>
            <RefreshCw className="h-4 w-4" />
            {t('action.refresh')}
          </Button>
          {/* T-27：统一事件历史（三型触发器的结构化记录聚合视图） */}
          <Button variant="outline" size="sm" onClick={() => setEventsOpen(true)}>
            <History className="h-4 w-4" />
            {t('events.button')}
          </Button>
          {tab === 'watchers' ? (
            <Button size="sm" onClick={openCreate}>
              <Plus className="h-4 w-4" />
              {t('action.new')}
            </Button>
          ) : null}
        </>
      }
    >
      <SegmentedTabs
        items={tabItems}
        value={tab}
        onChange={(v) => setTab(v)}
        ariaLabel={t('tabs.aria')}
        className="mb-4 w-fit"
      />

      {tab !== 'watchers' ? (
        tab === 'schedules' ? (
          <SchedulesPanel refreshSignal={panelRefresh} />
        ) : (
          <WebhooksPanel refreshSignal={panelRefresh} />
        )
      ) : rules === null ? (
        <div className="space-y-2">
          {Array.from({ length: 3 }).map((_, i) => (
            <Skeleton key={i} className="h-14 w-full" />
          ))}
        </div>
      ) : rules.length === 0 ? (
        <div className="flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
          <BellRing className="h-10 w-10 text-muted-foreground/50" />
          <div>
            <p className="font-medium">{t('empty.title')}</p>
            <p className="mt-1 text-sm text-muted-foreground">
              {t('empty.description')}
            </p>
          </div>
          <Button size="sm" onClick={openCreate}>
            <Plus className="h-4 w-4" />
            {t('action.new')}
          </Button>
        </div>
      ) : (
        <div className="overflow-x-auto rounded-lg border border-border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t('table.name')}</TableHead>
                <TableHead>{t('table.watchDir')}</TableHead>
                <TableHead>{t('table.action')}</TableHead>
                <TableHead className="w-20">{t('table.enabled')}</TableHead>
                <TableHead>{t('table.recent')}</TableHead>
                <TableHead className="w-36 text-right">
                  {t('table.actions')}
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rules.map((rule) => (
                <TableRow key={rule.id}>
                  <TableCell>
                    <span className="font-medium">{rule.name}</span>
                    <span className="ml-2 font-mono text-[10px] text-muted-foreground">
                      {rule.id}
                    </span>
                  </TableCell>
                  <TableCell className="max-w-56">
                    <span
                      className="block truncate font-mono text-xs text-muted-foreground"
                      title={rule.watch_dir}
                    >
                      {rule.watch_dir}
                    </span>
                  </TableCell>
                  <TableCell>
                    <Badge variant="outline" className="max-w-64 font-normal">
                      <span className="truncate">{modeSummary(rule, t)}</span>
                    </Badge>
                  </TableCell>
                  <TableCell>
                    <Switch
                      checked={rule.enabled}
                      onCheckedChange={(v) => void handleToggle(rule, v)}
                      aria-label={`${t('table.enabled')}: ${rule.name}`}
                      title={rule.enabled ? undefined : t('toggle.disabledHint')}
                    />
                  </TableCell>
                  <TableCell>
                    {rule.recent && rule.recent.length > 0 ? (
                      <div className="space-y-0.5">
                        {rule.recent.slice(0, RECENT_PREVIEW_MAX).map((r, idx) => (
                          <div
                            key={idx}
                            className="flex items-center gap-1.5 text-[11px] text-muted-foreground"
                          >
                            <span
                              className={cn(
                                'h-1.5 w-1.5 shrink-0 rounded-full',
                                statusDotClass(r.status),
                              )}
                            />
                            <span className="max-w-40 truncate" title={r.file}>
                              {basename(r.file)}
                            </span>
                          </div>
                        ))}
                        {rule.recent.length > RECENT_PREVIEW_MAX ? (
                          <button
                            type="button"
                            onClick={() => setHistoryRule(rule)}
                            className="text-[11px] text-muted-foreground underline-offset-2 hover:underline"
                          >
                            {t('recent.more', {
                              count: rule.recent.length - RECENT_PREVIEW_MAX,
                            })}
                          </button>
                        ) : null}
                      </div>
                    ) : (
                      <span className="text-xs text-muted-foreground">
                        {t('recent.none')}
                      </span>
                    )}
                  </TableCell>
                  <TableCell className="text-right">
                    <div className="flex items-center justify-end gap-1">
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => setHistoryRule(rule)}
                        aria-label={t('action.history')}
                        title={t('action.history')}
                      >
                        <History className="h-4 w-4" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => openEdit(rule)}
                        aria-label={t('action.edit')}
                        title={t('action.edit')}
                      >
                        <Pencil className="h-4 w-4" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => setDeleteTarget(rule)}
                        aria-label={t('action.delete')}
                        title={t('action.delete')}
                        className="text-destructive hover:text-destructive"
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}

      <RuleDialog
        open={editorOpen}
        onOpenChange={setEditorOpen}
        rule={editing}
        onSaved={reload}
      />
      <HistoryDialog
        rule={historyRule}
        onOpenChange={(open) => {
          if (!open) setHistoryRule(null)
        }}
      />
      {eventsOpen ? (
        <EventsDialog preset={null} onOpenChange={() => setEventsOpen(false)} />
      ) : null}

      {/* 删除确认 */}
      <Dialog
        open={deleteTarget !== null}
        onOpenChange={(v) => {
          if (!v) setDeleteTarget(null)
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t('delete.title')}</DialogTitle>
            <DialogDescription>{t('delete.message', { name: deleteTarget?.name ?? '' })}</DialogDescription>
          </DialogHeader>
          <DialogFooter className="gap-2">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setDeleteTarget(null)}
              disabled={deleting}
            >
              {t('common:action.cancel')}
            </Button>
            <Button
              size="sm"
              variant="destructive"
              onClick={() => void handleDelete()}
              disabled={deleting}
            >
              {deleting ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
              {t('action.delete')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  )
}
