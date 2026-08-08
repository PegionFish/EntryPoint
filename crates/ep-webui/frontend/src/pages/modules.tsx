import { useEffect, useMemo, useRef, useState } from 'react'
import {
  ChevronDown,
  CircleCheck,
  CircleX,
  Database,
  Download,
  FileArchive,
  FileBox,
  FolderOpen,
  Gauge,
  Globe,
  HardDrive,
  Loader2,
  MemoryStick,
  Package,
  PackagePlus,
  Pencil,
  Play,
  RefreshCw,
  ScrollText,
  SlidersHorizontal,
  Square,
  Tag,
  Trash2,
  TriangleAlert,
  Upload,
  X,
  Zap,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { api, uploadModelWithProgress } from '@/api/client'
import type {
  CapabilityDecl,
  CapabilityParamSchema,
  ModelInfo,
  ModelListResponse,
  ModelSource,
  ModuleResponse,
  PipelineSummary,
} from '@/api/types'
import { wsManager } from '@/api/ws'
import { PageContainer } from '@/components/layout/page-container'
import { ConfirmDialog } from '@/components/shared/confirm-dialog'
import { EmptyState } from '@/components/shared/empty-state'
import { LogViewer } from '@/components/shared/log-viewer'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Progress } from '@/components/ui/progress'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs'
import { isTaskTerminal, useDirectExec } from '@/hooks/use-direct-exec'
import {
  useModelDownloads,
  type ModelDownloadProgress,
} from '@/hooks/use-model-downloads'
import { useModels } from '@/hooks/use-models'
import { useModules } from '@/hooks/use-modules'
import {
  PACK_UPLOAD_ABORTED,
  usePackIo,
  type PackProgressEntry,
  type PackUploadProgress,
  type UsePackIoResult,
} from '@/hooks/use-pack-io'
import { categoryLabel, statusMeta } from '@/lib/constants'
import { cn, formatBytes, formatMB } from '@/lib/utils'

// ─── 常量与工具 ──────────────────────────────────────────────────────────────

/** 翻译函数签名（与 react-i18next useTranslation 返回的 t 兼容） */
type TranslateFn = (key: string, options?: Record<string, unknown>) => string

/** pack id 语法（`<publisher>.<pack-name>`，两段 lowercase 字母数字-） */
const PACK_ID_PATTERN = /^[a-z0-9][a-z0-9-]*\.[a-z0-9][a-z0-9-]*$/

/**
 * 从 apiFetch 抛出的 `API <status>: <body>` 中提取可读错误。
 * 后端错误体形如 {"error":"中文"}，直接展示原始 JSON 不友好。
 */
function errMsg(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e)
  // 直跑提交超时哨兵（use-direct-exec 抛出）
  if (raw === '__ep_direct_exec_submit_timeout__') return raw
  const m = raw.match(/^API \d+: (.*)$/s)
  const body = m ? m[1] : raw
  try {
    const parsed: unknown = JSON.parse(body)
    if (parsed && typeof parsed === 'object' && 'error' in parsed) {
      const msg = (parsed as { error: unknown }).error
      if (typeof msg === 'string' && msg.trim()) return msg
    }
  } catch {
    // body 不是 JSON：原样返回
  }
  return body
}

function normalizeStatus(status: string): string {
  return status.trim().toLowerCase()
}

function isReadyStatus(status: string): boolean {
  return normalizeStatus(status) === 'ready'
}

/** 模型来源 → 展示标签（品牌名 HuggingFace / ModelScope 保持原文，其余走翻译） */
function sourceLabel(t: TranslateFn, source: string): string {
  switch (source.toLowerCase()) {
    case 'huggingface':
      return 'HuggingFace'
    case 'modelscope':
      return 'ModelScope'
    case 'url':
      return t('models:source.url')
    case 'local_import':
      return t('models:source.localImport')
    case 'pack':
      return t('models:source.pack', { defaultValue: '整合包' })
    default:
      return source
  }
}

/** 收窄字符串来源为 ModelSource（后端 source 字段可能为 local_import 等） */
function asModelSource(source: string | undefined): ModelSource | undefined {
  return source === 'huggingface' || source === 'modelscope' || source === 'url'
    ? source
    : undefined
}

/** 模型主下载源：available_sources 首位为主源，缺失时回退 source 字段 */
function primarySource(model: ModelInfo): ModelSource | undefined {
  return asModelSource(model.available_sources?.[0] ?? model.source)
}

/** tag 客户端归一化（服务端同款语义：trim、去空、保序去重） */
function normalizeTags(tags: string[]): string[] {
  const out: string[] = []
  for (const raw of tags) {
    const tag = raw.trim()
    if (!tag || out.includes(tag)) continue
    out.push(tag)
  }
  return out
}

// ─── 状态徽章 ────────────────────────────────────────────────────────────────

interface ModelStatusMeta {
  /** 状态文案翻译键（复用 common:status.*）；null 表示原样展示后端状态值 */
  labelKey: string | null
  dot: string
  badge: string
  transitional: boolean
}

/** 模型状态 → 元信息（ready=绿 / missing=红 / incomplete=黄 / downloading=蓝） */
function modelStatusMeta(status: string): ModelStatusMeta {
  switch (status.trim().toLowerCase()) {
    case 'ready':
      return {
        labelKey: 'common:status.ready',
        dot: 'bg-status-running',
        badge: 'bg-status-running/15 text-status-running border-status-running/30',
        transitional: false,
      }
    case 'missing':
      return {
        labelKey: 'common:status.missing',
        dot: 'bg-status-error',
        badge: 'bg-status-error/15 text-status-error border-status-error/30',
        transitional: false,
      }
    case 'incomplete':
      return {
        labelKey: 'common:status.incomplete',
        dot: 'bg-status-preparing',
        badge:
          'bg-status-preparing/15 text-status-preparing border-status-preparing/30',
        transitional: false,
      }
    case 'downloading':
      return {
        labelKey: 'common:status.downloading',
        dot: 'bg-status-starting',
        badge:
          'bg-status-starting/15 text-status-starting border-status-starting/30',
        transitional: true,
      }
    default:
      return {
        labelKey: null,
        dot: 'bg-muted-foreground',
        badge: 'bg-muted text-muted-foreground border-border',
        transitional: false,
      }
  }
}

function ModelStatusBadge({ status }: { status: string }) {
  const { t } = useTranslation('modules')
  const meta = modelStatusMeta(status)
  return (
    <Badge variant="outline" className={meta.badge}>
      <span
        className={cn(
          'size-1.5 rounded-full',
          meta.dot,
          meta.transitional && 'animate-pulse',
        )}
      />
      {meta.labelKey ? t(meta.labelKey) : status}
    </Badge>
  )
}

/** 模块服务状态徽章（running/stopped/starting/…，复用 constants 状态元） */
function ServiceStatusBadge({ status }: { status: string }) {
  const { t } = useTranslation('modules')
  const meta = statusMeta(status)
  const label = meta.labelKey ? t(meta.labelKey) : status
  return (
    <Badge variant="outline" className={meta.badge}>
      <span
        className={cn(
          'size-1.5 rounded-full',
          meta.dot,
          meta.transitional && 'animate-pulse',
        )}
      />
      {label}
    </Badge>
  )
}

// ─── 模块日志抽屉（历史 + WS 实时）─────────────────────────────────────────

/** 前端日志缓冲上限（行） */
const MAX_LOG_LINES = 1000

/** 打开期间拉取历史日志 + 订阅 WS 实时日志（按 module_id 过滤） */
function useModuleLogs(moduleId: string | null): string[] {
  const [lines, setLines] = useState<string[]>([])
  useEffect(() => {
    if (!moduleId) return
    setLines([])
    let cancelled = false
    api
      .moduleLogs(moduleId)
      .then((res) => {
        if (!cancelled) setLines(res.lines ?? [])
      })
      .catch(() => {
        // 拉取失败保持空列表，等待实时日志
      })
    const off = wsManager.onMessage((msg) => {
      if (msg.type !== 'log' || msg.module_id !== moduleId) return
      setLines((prev) => {
        const next =
          prev.length >= MAX_LOG_LINES
            ? prev.slice(prev.length - MAX_LOG_LINES + 1)
            : prev.slice()
        next.push(msg.line)
        return next
      })
    })
    return () => {
      cancelled = true
      off()
    }
  }, [moduleId])
  return lines
}

// ─── 直跑抽屉 ───────────────────────────────────────────────────────────────

/** 直跑参数表单：按 ParamSchema 类型渲染控件，提交前按类型归一 */
function ParamField({
  name,
  schema,
  value,
  onChange,
}: {
  name: string
  schema: CapabilityParamSchema
  value: unknown
  onChange: (value: unknown) => void
}) {
  const enumValues = schema.enum ?? schema.options ?? null
  const type = (schema.type || 'string').toLowerCase()

  if (enumValues && enumValues.length > 0) {
    const current = value === undefined || value === null ? '' : String(value)
    return (
      <Select value={current} onValueChange={(v) => onChange(v)}>
        <SelectTrigger className="w-full">
          <SelectValue placeholder={name} />
        </SelectTrigger>
        <SelectContent>
          {enumValues.map((opt) => (
            <SelectItem key={opt} value={opt}>
              {opt}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    )
  }

  if (type === 'boolean') {
    const current = value === undefined || value === null ? '' : String(value)
    return (
      <Select value={current} onValueChange={(v) => onChange(v === 'true')}>
        <SelectTrigger className="w-full">
          <SelectValue placeholder={name} />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="true">true</SelectItem>
          <SelectItem value="false">false</SelectItem>
        </SelectContent>
      </Select>
    )
  }

  if (type === 'integer' || type === 'float' || type === 'number') {
    return (
      <Input
        type="number"
        value={value === undefined || value === null ? '' : String(value)}
        min={schema.min ?? undefined}
        max={schema.max ?? undefined}
        step={schema.step ?? (type === 'integer' ? 1 : 'any')}
        onChange={(e) => {
          const raw = e.target.value
          if (raw === '') {
            onChange(undefined)
            return
          }
          const num =
            type === 'integer' ? Number.parseInt(raw, 10) : Number.parseFloat(raw)
          onChange(Number.isNaN(num) ? raw : num)
        }}
        className="font-mono text-xs"
      />
    )
  }

  return (
    <Input
      value={value === undefined || value === null ? '' : String(value)}
      onChange={(e) => onChange(e.target.value)}
      className="font-mono text-xs"
    />
  )
}

/** 直跑产物预览（文本 / 图片；其余类型仅下载） */
interface ArtifactPreview {
  nodeId: string
  name: string
  kind: 'text' | 'image' | 'binary'
  text?: string
  objectUrl?: string
  size: number
}

const TEXT_PREVIEW_EXTS = /\.(txt|json|srt|vtt|ass|csv|log|md|toml)$/i
const IMAGE_PREVIEW_EXTS = /\.(png|jpe?g|webp|gif|bmp)$/i
/** 预览大小上限（超过仅下载） */
const PREVIEW_MAX_BYTES = 2 * 1024 * 1024

async function fetchArtifactPreview(
  url: string,
  nodeId: string,
  name: string,
): Promise<ArtifactPreview> {
  const resp = await fetch(url)
  if (!resp.ok) throw new Error(`API ${resp.status}`)
  // P1：先读 Content-Length 预判体积，超限直接取消 body 放弃预览。
  // 避免 await resp.blob() 把数 GB 产物全量下载进内存造成内存峰值。
  const headerLen = Number(resp.headers.get('Content-Length'))
  if (Number.isFinite(headerLen) && headerLen > PREVIEW_MAX_BYTES) {
    void resp.body?.cancel()
    return { nodeId, name, kind: 'binary', size: headerLen }
  }
  const blob = await resp.blob()
  if (blob.size > PREVIEW_MAX_BYTES) {
    return { nodeId, name, kind: 'binary', size: blob.size }
  }
  if (IMAGE_PREVIEW_EXTS.test(name)) {
    return {
      nodeId,
      name,
      kind: 'image',
      objectUrl: URL.createObjectURL(blob),
      size: blob.size,
    }
  }
  if (TEXT_PREVIEW_EXTS.test(name)) {
    return { nodeId, name, kind: 'text', text: await blob.text(), size: blob.size }
  }
  return { nodeId, name, kind: 'binary', size: blob.size }
}

/** 直跑抽屉节点 id → 展示名（退化三节点 DAG，build_direct_pipeline 契约） */
const DIRECT_NODE_LABEL_KEYS: Record<string, string> = {
  input: 'models:run.nodeInput',
  run: 'models:run.nodeRun',
  output: 'models:run.nodeOutput',
}

interface DirectRunDrawerProps {
  module: ModuleResponse | null
  onClose: () => void
}

/**
 * 单模型直跑抽屉：
 * 选 capability（裸名，来自 ModuleResponse.capabilities）→ 按 params schema
 * 渲染参数表单（预填 default）→ 输入文件（本地路径 / 浏览器上传回填 path）
 * → executeSingle（后端未运行时自动拉起模块并同步等健康，给足 fetch 超时）
 * → WS progress 按 task_id 过滤进度 → 产物预览/下载。
 */
function DirectRunDrawer({ module, onClose }: DirectRunDrawerProps) {
  const { t } = useTranslation('modules')
  const capabilities = useMemo(() => module?.capabilities ?? [], [module])
  const [capability, setCapability] = useState('')
  const [params, setParams] = useState<Record<string, unknown>>({})
  const [inputPath, setInputPath] = useState('')
  const [uploadingInput, setUploadingInput] = useState(false)
  const [preview, setPreview] = useState<ArtifactPreview | null>(null)
  const [previewError, setPreviewError] = useState<string | null>(null)
  const fileInputRef = useRef<HTMLInputElement | null>(null)
  const exec = useDirectExec()

  // 打开新模块时重置表单：capability 缺省取第一项，参数预填 schema 默认值
  useEffect(() => {
    exec.reset()
    setInputPath('')
    setPreview(null)
    setPreviewError(null)
    const first = capabilities[0]
    setCapability(first?.name ?? '')
    setParams(defaultParamsOf(first))
    // exec.reset 为稳定引用；仅在切换模块时重置
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [module?.id])

  function defaultParamsOf(cap: CapabilityDecl | undefined): Record<string, unknown> {
    const out: Record<string, unknown> = {}
    if (!cap?.params) return out
    for (const [name, schema] of Object.entries(cap.params)) {
      if (schema.default !== undefined && schema.default !== null) {
        out[name] = schema.default
      }
    }
    return out
  }

  function handleCapabilityChange(name: string) {
    setCapability(name)
    setParams(defaultParamsOf(capabilities.find((c) => c.name === name)))
  }

  async function handleUploadInput(file: File) {
    setUploadingInput(true)
    try {
      const resp = await api.uploadInput(file)
      setInputPath(resp.path)
      toast.success(t('models:run.uploadDone', { defaultValue: '输入文件已上传' }), {
        description: resp.path,
      })
    } catch (e) {
      toast.error(t('models:run.uploadFailed', { defaultValue: '输入文件上传失败' }), {
        description: errMsg(e),
      })
    } finally {
      setUploadingInput(false)
    }
  }

  async function handleSubmit() {
    if (!module || !capability || !inputPath.trim() || exec.submitting) return
    const task = await exec.submit({
      module_id: module.id,
      capability,
      params,
      input_path: inputPath.trim(),
    })
    if (task) {
      toast.success(t('models:run.accepted', { defaultValue: '直跑任务已提交' }), {
        description: task,
      })
    } else if (exec.submitError === '__ep_direct_exec_submit_timeout__') {
      toast.error(t('models:run.submitTimeout', { defaultValue: '等待模块启动超时' }), {
        description: t('models:run.submitTimeoutDesc', {
          defaultValue: '模块自动拉起耗时超出预期，可稍后在任务页查看或重试',
        }),
      })
    } else if (exec.submitError) {
      toast.error(t('models:run.submitFailed', { defaultValue: '直跑提交失败' }), {
        description: errMsg(exec.submitError),
      })
    }
  }

  async function handlePreview(nodeId: string, name: string) {
    if (!exec.taskId) return
    setPreviewError(null)
    try {
      const p = await fetchArtifactPreview(
        api.taskArtifactUrl(exec.taskId, nodeId),
        nodeId,
        name,
      )
      setPreview(p)
    } catch (e) {
      setPreviewError(e instanceof Error ? e.message : String(e))
    }
  }

  // 预览对象 URL 生命周期管理
  useEffect(() => {
    return () => {
      if (preview?.objectUrl) URL.revokeObjectURL(preview.objectUrl)
    }
  }, [preview])

  const currentCap = capabilities.find((c) => c.name === capability)
  const paramEntries = useMemo(
    () =>
      Object.entries(currentCap?.params ?? {}).sort(([a], [b]) =>
        a.localeCompare(b),
      ),
    [currentCap],
  )
  const taskStatus = exec.task?.status ?? null
  const terminal = isTaskTerminal(taskStatus)

  return (
    <Sheet
      open={module !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <SheetContent className="overflow-y-auto sm:max-w-xl">
        <SheetHeader>
          <SheetTitle className="flex items-center gap-2">
            <Zap className="size-4 text-primary" />
            {t('models:run.title', { defaultValue: '单模型直跑' })}
            {module ? (
              <span className="text-muted-foreground">· {module.name}</span>
            ) : null}
          </SheetTitle>
          <SheetDescription>
            {t('models:run.description', {
              defaultValue: '选择能力与参数后直接执行，模块未运行时将自动拉起',
            })}
          </SheetDescription>
        </SheetHeader>

        {capabilities.length === 0 ? (
          <p className="px-4 text-sm text-muted-foreground">
            {t('models:run.noCapabilities', {
              defaultValue: '该模块未声明任何能力，无法直跑',
            })}
          </p>
        ) : (
          <div className="space-y-5 px-4 pb-6">
            {/* 1. 能力选择（裸名，来自 manifest capabilities） */}
            <div className="space-y-2">
              <label className="text-sm font-medium">
                {t('models:run.capability', { defaultValue: '能力' })}
              </label>
              <Select
                value={capability || undefined}
                onValueChange={handleCapabilityChange}
                disabled={exec.submitting}
              >
                <SelectTrigger className="w-full">
                  <SelectValue
                    placeholder={t('models:run.selectCapability', {
                      defaultValue: '选择能力',
                    })}
                  />
                </SelectTrigger>
                <SelectContent>
                  {capabilities.map((cap) => (
                    <SelectItem key={cap.name} value={cap.name}>
                      {cap.name}
                      {cap.description ? ` — ${cap.description}` : ''}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {currentCap ? (
                <p className="text-xs text-muted-foreground">
                  {currentCap.input_type} → {currentCap.output_type}
                  {currentCap.max_file_size_mb
                    ? ` · ≤ ${currentCap.max_file_size_mb} MB`
                    : ''}
                </p>
              ) : null}
            </div>

            {/* 2. 参数表单（type/default/min/max/enum 数据驱动） */}
            {paramEntries.length > 0 && (
              <div className="space-y-3">
                <label className="text-sm font-medium">
                  {t('models:run.params', { defaultValue: '参数' })}
                </label>
                {paramEntries.map(([name, schema]) => (
                  <div key={name} className="space-y-1">
                    <div className="flex items-center justify-between gap-2">
                      <span className="font-mono text-xs text-foreground/80">
                        {name}
                      </span>
                      <span className="text-[10px] text-muted-foreground">
                        {schema.type}
                        {schema.min !== null && schema.min !== undefined
                          ? ` · min ${schema.min}`
                          : ''}
                        {schema.max !== null && schema.max !== undefined
                          ? ` · max ${schema.max}`
                          : ''}
                      </span>
                    </div>
                    <ParamField
                      name={name}
                      schema={schema}
                      value={params[name]}
                      onChange={(v) => setParams((prev) => ({ ...prev, [name]: v }))}
                    />
                    {schema.description ? (
                      <p className="text-[11px] text-muted-foreground">
                        {schema.description}
                      </p>
                    ) : null}
                  </div>
                ))}
              </div>
            )}

            {/* 3. 输入文件：本地路径直填 或 浏览器上传回填 path */}
            <div className="space-y-2">
              <label className="text-sm font-medium">
                {t('models:run.input', { defaultValue: '输入文件' })}
              </label>
              <div className="flex gap-2">
                <Input
                  value={inputPath}
                  onChange={(e) => setInputPath(e.target.value)}
                  placeholder={t('models:run.inputPlaceholder', {
                    defaultValue: '服务器本地路径，如 /path/to/input.wav',
                  })}
                  className="font-mono text-xs"
                  disabled={exec.submitting}
                />
                <Button
                  variant="outline"
                  size="sm"
                  className="shrink-0"
                  disabled={uploadingInput || exec.submitting}
                  onClick={() => fileInputRef.current?.click()}
                >
                  {uploadingInput ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : (
                    <Upload className="size-3.5" />
                  )}
                  {t('models:run.upload', { defaultValue: '上传' })}
                </Button>
                <input
                  ref={fileInputRef}
                  type="file"
                  className="hidden"
                  onChange={(e) => {
                    const file = e.target.files?.[0]
                    if (file) void handleUploadInput(file)
                    e.target.value = ''
                  }}
                />
              </div>
              <p className="text-[11px] text-muted-foreground">
                {t('models:run.inputHint', {
                  defaultValue:
                    '填写服务器上已存在的文件路径；或点「上传」把浏览器文件暂存到服务器后自动回填路径',
                })}
              </p>
            </div>

            {/* 4. 提交 */}
            <div className="space-y-2">
              <Button
                className="w-full"
                disabled={!capability || !inputPath.trim() || exec.submitting}
                onClick={() => void handleSubmit()}
              >
                {exec.submitting ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <Play className="size-4" />
                )}
                {exec.submitting
                  ? t('models:run.startingModule', {
                      defaultValue: '正在启动模块并提交任务…',
                    })
                  : t('models:run.submit', { defaultValue: '执行' })}
              </Button>
              {exec.submitting && (
                <p className="text-[11px] text-muted-foreground">
                  {t('models:run.startingModuleHint', {
                    defaultValue:
                      '模块未运行时后端会自动拉起并等待就绪（默认最长 30 秒，首次运行需准备环境可能更久），请保持页面打开',
                  })}
                </p>
              )}
            </div>

            {/* 5. 任务进度（WS progress 按 task_id 过滤 + 详情轮询） */}
            {exec.taskId && (
              <div className="space-y-2 rounded-lg border border-border bg-muted/30 p-3">
                <div className="flex items-center justify-between gap-2">
                  <span className="font-mono text-xs text-muted-foreground">
                    {exec.taskId}
                  </span>
                  <Badge
                    variant="outline"
                    className={cn(
                      'border-border',
                      taskStatus === 'completed' &&
                        'border-status-running/30 bg-status-running/15 text-status-running',
                      taskStatus === 'failed' &&
                        'border-status-error/30 bg-status-error/15 text-status-error',
                      (taskStatus === 'running' || taskStatus === 'queued') &&
                        'border-status-starting/30 bg-status-starting/15 text-status-starting',
                      taskStatus === 'cancelled' &&
                        'border-status-preparing/30 bg-status-preparing/15 text-status-preparing',
                    )}
                  >
                    {taskStatus
                      ? t(
                          `common:status.${taskStatus === 'queued' ? 'pending' : taskStatus}`,
                          { defaultValue: taskStatus },
                        )
                      : '…'}
                  </Badge>
                </div>
                {exec.task?.nodes
                  .filter((n) => n.error)
                  .map((n) => (
                    <p key={n.node_id} className="break-all text-xs text-status-error">
                      {n.node_id}: {n.error}
                    </p>
                  ))}
                <div className="space-y-1">
                  {Object.entries(DIRECT_NODE_LABEL_KEYS).map(([nodeId, key]) => {
                    const nodeState =
                      exec.task?.nodes.find((n) => n.node_id === nodeId)?.state ??
                      exec.nodeProgress[nodeId]
                    return (
                      <div
                        key={nodeId}
                        className="flex items-center justify-between gap-2 text-xs"
                      >
                        <span className="text-muted-foreground">
                          {t(key, {
                            defaultValue:
                              nodeId === 'input'
                                ? '输入'
                                : nodeId === 'run'
                                  ? '运行'
                                  : '输出',
                          })}
                        </span>
                        <span
                          className={cn(
                            'font-mono',
                            nodeState === 'completed' && 'text-status-running',
                            nodeState === 'failed' && 'text-status-error',
                            nodeState === 'running' && 'text-status-starting',
                          )}
                        >
                          {nodeState ?? '—'}
                        </span>
                      </div>
                    )
                  })}
                </div>
                {!terminal && (
                  <p className="text-[11px] text-muted-foreground">
                    {t('models:run.progressHint', {
                      defaultValue: '任务执行中，进度实时更新；可在任务页查看完整历史',
                    })}
                  </p>
                )}
              </div>
            )}

            {/* 6. 产物预览 / 下载 */}
            {terminal && exec.artifacts.length > 0 && (
              <div className="space-y-2">
                <label className="text-sm font-medium">
                  {t('models:run.artifacts', { defaultValue: '结果产物' })}
                </label>
                <div className="divide-y divide-border rounded-lg border border-border">
                  {exec.artifacts.map((artifact) => (
                    <div
                      key={`${artifact.node_id}-${artifact.name}`}
                      className="flex flex-wrap items-center gap-2 px-3 py-2"
                    >
                      <FileBox className="size-3.5 shrink-0 text-muted-foreground" />
                      <span className="min-w-0 flex-1 truncate font-mono text-xs">
                        {artifact.name}
                      </span>
                      <span className="text-[10px] text-muted-foreground">
                        {formatBytes(artifact.size)}
                      </span>
                      <Button
                        variant="ghost"
                        size="xs"
                        onClick={() => void handlePreview(artifact.node_id, artifact.name)}
                      >
                        {t('models:run.preview', { defaultValue: '预览' })}
                      </Button>
                      <a
                        href={
                          exec.taskId
                            ? api.taskArtifactUrl(exec.taskId, artifact.node_id)
                            : '#'
                        }
                        download
                      >
                        <Button variant="outline" size="xs">
                          <Download className="size-3" />
                          {t('common:action.download')}
                        </Button>
                      </a>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
        )}

        {/* 产物预览对话框 */}
        <Dialog
          open={preview !== null || previewError !== null}
          onOpenChange={(open) => {
            if (!open) {
              if (preview?.objectUrl) URL.revokeObjectURL(preview.objectUrl)
              setPreview(null)
              setPreviewError(null)
            }
          }}
        >
          <DialogContent className="sm:max-w-2xl">
            <DialogHeader>
              <DialogTitle className="font-mono text-sm">
                {preview?.name ?? t('models:run.preview', { defaultValue: '预览' })}
              </DialogTitle>
              <DialogDescription>
                {preview ? formatBytes(preview.size) : ''}
              </DialogDescription>
            </DialogHeader>
            {previewError ? (
              <p className="text-sm text-status-error">
                {t('models:run.previewFailed', { defaultValue: '预览失败' })}：
                {previewError}
              </p>
            ) : preview?.kind === 'text' ? (
              <pre className="max-h-96 overflow-auto rounded-md border border-border bg-muted/30 p-3 font-mono text-xs leading-relaxed">
                {preview.text}
              </pre>
            ) : preview?.kind === 'image' && preview.objectUrl ? (
              <img
                src={preview.objectUrl}
                alt={preview.name}
                className="mx-auto max-h-96 rounded-md border border-border object-contain"
              />
            ) : (
              <p className="text-sm text-muted-foreground">
                {t('models:run.previewBinary', {
                  defaultValue: '该产物类型不支持预览，请直接下载',
                })}
              </p>
            )}
          </DialogContent>
        </Dialog>
      </SheetContent>
    </Sheet>
  )
}

// ─── 模块详情抽屉（能力 / 参数 schema）───────────────────────────────────────

function ModuleDetailSheet({
  module,
  onClose,
}: {
  module: ModuleResponse | null
  onClose: () => void
}) {
  const { t } = useTranslation('modules')
  const capabilities = module?.capabilities ?? []
  return (
    <Sheet
      open={module !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <SheetContent className="overflow-y-auto sm:max-w-lg">
        <SheetHeader>
          <SheetTitle className="flex items-center gap-2">
            <SlidersHorizontal className="size-4 text-primary" />
            {module?.name ?? ''}
            {t('models:detail.drawerSuffix', { defaultValue: ' · 模块详情' })}
          </SheetTitle>
          <SheetDescription className="font-mono text-xs">
            {module?.id}
            {module?.version ? ` · v${module.version}` : ''}
          </SheetDescription>
        </SheetHeader>
        <div className="space-y-4 px-4 pb-6">
          {module?.description ? (
            <p className="text-sm text-foreground/80">{module.description}</p>
          ) : null}
          <div className="space-y-1 text-xs text-muted-foreground">
            <p className="flex items-center gap-1.5">
              <FolderOpen className="size-3.5" />
              <span className="break-all font-mono">{module?.path}</span>
            </p>
          </div>

          <div className="space-y-2">
            <h3 className="text-sm font-semibold">
              {t('models:detail.capabilities', { defaultValue: '能力声明' })}
            </h3>
            {capabilities.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {t('models:detail.noCapabilities', {
                  defaultValue: '该模块未声明能力（manifest interface.capabilities 为空）',
                })}
              </p>
            ) : (
              capabilities.map((cap) => (
                <div
                  key={cap.name}
                  className="space-y-2 rounded-lg border border-border p-3"
                >
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-sm font-medium">{cap.name}</span>
                    <Badge
                      variant="outline"
                      className="px-1.5 text-[10px] font-normal text-muted-foreground"
                    >
                      {cap.input_type} → {cap.output_type}
                    </Badge>
                    {cap.supports_batch ? (
                      <Badge
                        variant="secondary"
                        className="px-1.5 text-[10px] font-normal"
                      >
                        batch
                      </Badge>
                    ) : null}
                  </div>
                  {cap.description ? (
                    <p className="text-xs text-muted-foreground">{cap.description}</p>
                  ) : null}
                  {cap.params && Object.keys(cap.params).length > 0 ? (
                    <div className="overflow-x-auto">
                      <table className="w-full text-left text-xs">
                        <thead>
                          <tr className="border-b border-border text-muted-foreground">
                            <th className="py-1 pr-3 font-normal">
                              {t('models:detail.paramName', { defaultValue: '参数' })}
                            </th>
                            <th className="py-1 pr-3 font-normal">
                              {t('models:detail.paramType', { defaultValue: '类型' })}
                            </th>
                            <th className="py-1 pr-3 font-normal">
                              {t('models:detail.paramDefault', { defaultValue: '默认值' })}
                            </th>
                            <th className="py-1 font-normal">
                              {t('models:detail.paramConstraint', { defaultValue: '约束' })}
                            </th>
                          </tr>
                        </thead>
                        <tbody>
                          {Object.entries(cap.params)
                            .sort(([a], [b]) => a.localeCompare(b))
                            .map(([name, schema]) => {
                              const enumValues = schema.enum ?? schema.options
                              return (
                                <tr key={name} className="border-b border-border/50">
                                  <td className="py-1.5 pr-3 font-mono align-top">
                                    {name}
                                    {schema.description ? (
                                      <p className="mt-0.5 font-sans text-[10px] text-muted-foreground">
                                        {schema.description}
                                      </p>
                                    ) : null}
                                  </td>
                                  <td className="py-1.5 pr-3 font-mono align-top">
                                    {schema.type}
                                  </td>
                                  <td className="py-1.5 pr-3 font-mono align-top">
                                    {schema.default === undefined ||
                                    schema.default === null
                                      ? '—'
                                      : JSON.stringify(schema.default)}
                                  </td>
                                  <td className="py-1.5 font-mono align-top text-muted-foreground">
                                    {enumValues && enumValues.length > 0
                                      ? enumValues.join(' | ')
                                      : [
                                            schema.min !== null && schema.min !== undefined
                                              ? `min ${schema.min}`
                                              : null,
                                            schema.max !== null && schema.max !== undefined
                                              ? `max ${schema.max}`
                                              : null,
                                          ]
                                            .filter(Boolean)
                                            .join(', ') || '—'}
                                  </td>
                                </tr>
                              )
                            })}
                        </tbody>
                      </table>
                    </div>
                  ) : null}
                </div>
              ))
            )}
          </div>
        </div>
      </SheetContent>
    </Sheet>
  )
}

// ─── 日志抽屉 ────────────────────────────────────────────────────────────────

function ModuleLogsSheet({
  moduleId,
  moduleName,
  onClose,
}: {
  moduleId: string | null
  moduleName: string
  onClose: () => void
}) {
  const { t } = useTranslation('modules')
  const lines = useModuleLogs(moduleId)
  return (
    <Sheet
      open={moduleId !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <SheetContent className="sm:max-w-2xl">
        <SheetHeader>
          <SheetTitle className="flex items-center gap-2">
            <ScrollText className="size-4 text-primary" />
            {t('models:logs.title', { defaultValue: '模块日志' })} · {moduleName}
          </SheetTitle>
          <SheetDescription>
            {t('models:logs.description', {
              defaultValue: '历史日志 + 实时推送（WebSocket）',
            })}
          </SheetDescription>
        </SheetHeader>
        <div className="px-4 pb-6">
          <LogViewer lines={lines} maxHeight={520} exportName={`${moduleId}-logs.txt`} />
        </div>
      </SheetContent>
    </Sheet>
  )
}

// ─── tag 编辑对话框 ──────────────────────────────────────────────────────────

interface TagsTarget {
  moduleId: string
  model: ModelInfo
}

function TagEditorDialog({
  target,
  onClose,
  onSaved,
}: {
  target: TagsTarget | null
  onClose: () => void
  onSaved: () => void
}) {
  const { t } = useTranslation('modules')
  const [tags, setTags] = useState<string[]>([])
  const [draft, setDraft] = useState('')
  const [saving, setSaving] = useState(false)

  useEffect(() => {
    setTags(normalizeTags(target?.model.tags ?? []))
    setDraft('')
  }, [target])

  function addDraft() {
    const next = normalizeTags([...tags, ...draft.split(/[,，]/)])
    setTags(next)
    setDraft('')
  }

  async function save() {
    if (!target || saving) return
    setSaving(true)
    try {
      await api.setModelTags(target.moduleId, target.model.model_id, {
        tags: normalizeTags(tags),
      })
      toast.success(t('models:tags.saved', { defaultValue: '标签已保存' }), {
        description: target.model.name,
      })
      onSaved()
      onClose()
    } catch (e) {
      toast.error(t('models:tags.saveFailed', { defaultValue: '保存标签失败' }), {
        description: errMsg(e),
      })
    } finally {
      setSaving(false)
    }
  }

  return (
    <Dialog
      open={target !== null}
      onOpenChange={(open) => {
        if (!open && !saving) onClose()
      }}
    >
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Tag className="size-4 text-primary" />
            {t('models:tags.editTitle', { defaultValue: '编辑标签' })}
          </DialogTitle>
          <DialogDescription>
            {t('models:tags.editDescription', {
              defaultValue:
                '标签存入模型元数据，随整合包流转；保存为全量覆写（空列表 = 清空）',
            })}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div className="flex min-h-9 flex-wrap items-center gap-1.5 rounded-md border border-border bg-muted/20 p-2">
            {tags.length === 0 ? (
              <span className="text-xs text-muted-foreground">
                {t('models:tags.empty', { defaultValue: '暂无标签' })}
              </span>
            ) : (
              tags.map((tag) => (
                <Badge key={tag} variant="secondary" className="gap-1 pr-1">
                  {tag}
                  <button
                    type="button"
                    className="rounded-full p-0.5 hover:bg-muted-foreground/20"
                    onClick={() => setTags(tags.filter((x) => x !== tag))}
                    aria-label={`${t('common:action.delete')} ${tag}`}
                  >
                    <X className="size-2.5" />
                  </button>
                </Badge>
              ))
            )}
          </div>
          <div className="flex gap-2">
            <Input
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault()
                  addDraft()
                }
              }}
              placeholder={t('models:tags.placeholder', {
                defaultValue: '输入标签后回车添加（逗号分隔可批量）',
              })}
              className="text-xs"
            />
            <Button
              variant="outline"
              size="sm"
              className="shrink-0"
              disabled={!draft.trim()}
              onClick={addDraft}
            >
              {t('models:tags.add', { defaultValue: '添加' })}
            </Button>
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={saving}>
            {t('common:action.cancel')}
          </Button>
          <Button onClick={() => void save()} disabled={saving}>
            {saving ? <Loader2 className="size-4 animate-spin" /> : null}
            {t('common:action.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ─── 整合包来源徽章（徽章菜单：卸载来源整合包）──────────────────────────────

function PackSourceBadge({
  packId,
  onUninstall,
}: {
  packId: string
  onUninstall: (packId: string) => void
}) {
  const { t } = useTranslation('modules')
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="shrink-0 rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
          title={t('card.packSource', { defaultValue: '整合包来源' })}
        >
          <Badge
            variant="outline"
            className="gap-1 cursor-pointer border-border/60 px-1.5 text-[10px] font-normal text-muted-foreground hover:border-primary/40 hover:text-foreground"
          >
            <Package className="size-2.5" />
            {packId}
          </Badge>
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem
          className="text-destructive focus:text-destructive"
          onSelect={() => onUninstall(packId)}
        >
          <Trash2 className="size-3.5" />
          {t('card.uninstallPack', { defaultValue: '卸载来源整合包' })}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

// ─── 导入模块对话框（本地路径 / URL / 浏览器上传）───────────────────────────

function ImportModuleDialog({
  open,
  onClose,
  io,
}: {
  open: boolean
  onClose: () => void
  io: UsePackIoResult
}) {
  const { t } = useTranslation('modules')
  const [localPath, setLocalPath] = useState('')
  const [url, setUrl] = useState('')
  const [pickedFile, setPickedFile] = useState<File | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const [uploadProgress, setUploadProgress] = useState<PackUploadProgress | null>(
    null,
  )
  const fileInputRef = useRef<HTMLInputElement | null>(null)
  const abortRef = useRef<(() => void) | null>(null)

  // 关闭时复位（中止进行中的上传）
  useEffect(() => {
    if (!open) {
      abortRef.current?.()
      abortRef.current = null
      setSubmitting(false)
      setUploadProgress(null)
      setPickedFile(null)
      setLocalPath('')
      setUrl('')
    }
  }, [open])

  async function submitLocal() {
    if (!localPath.trim() || submitting) return
    setSubmitting(true)
    try {
      const resp = await io.importLocal(localPath.trim())
      toast.success(t('importModule.accepted', { defaultValue: '模块导入已受理' }), {
        description: resp.pack_id,
      })
      onClose()
    } catch (e) {
      toast.error(t('importModule.failed', { defaultValue: '导入受理失败' }), {
        description: errMsg(e),
      })
    } finally {
      setSubmitting(false)
    }
  }

  async function submitUrl() {
    if (!url.trim() || submitting) return
    setSubmitting(true)
    try {
      const resp = await io.importUrl(url.trim())
      toast.success(t('importModule.accepted', { defaultValue: '模块导入已受理' }), {
        description: resp.pack_id,
      })
      onClose()
    } catch (e) {
      toast.error(t('importModule.failed', { defaultValue: '导入受理失败' }), {
        description: errMsg(e),
      })
    } finally {
      setSubmitting(false)
    }
  }

  async function submitUpload() {
    if (!pickedFile || submitting) return
    setSubmitting(true)
    setUploadProgress({ percent: 0, loaded: 0, total: pickedFile.size })
    const { promise, abort } = io.upload(pickedFile, setUploadProgress)
    abortRef.current = abort
    try {
      const resp = await promise
      toast.success(t('importModule.accepted', { defaultValue: '模块导入已受理' }), {
        description: resp.pack_id,
      })
      onClose()
    } catch (e) {
      if (e instanceof Error && e.message === PACK_UPLOAD_ABORTED) {
        toast.info(t('packs:toast.uploadCancelled', { defaultValue: '上传已取消' }))
      } else {
        toast.error(t('importModule.failed', { defaultValue: '导入受理失败' }), {
          description: errMsg(e),
        })
      }
    } finally {
      abortRef.current = null
      setSubmitting(false)
      setUploadProgress(null)
    }
  }

  return (
    <Dialog open={open} onOpenChange={(v) => (v ? undefined : onClose())}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Upload className="size-4 text-primary" />
            {t('toolbar.import', { defaultValue: '导入模块' })}
          </DialogTitle>
          <DialogDescription>
            {t('importModule.description', {
              defaultValue:
                '支持服务器本地 .epzip 路径、URL 下载与浏览器上传；受理后模块 + 模型 + 管线落位，进度实时推送',
            })}
          </DialogDescription>
        </DialogHeader>

        <Tabs defaultValue="local">
          <TabsList>
            <TabsTrigger value="local">
              <HardDrive className="size-3.5" />
              {t('packs:import.tabLocal', { defaultValue: '本地路径' })}
            </TabsTrigger>
            <TabsTrigger value="url">
              <Globe className="size-3.5" />
              URL
            </TabsTrigger>
            <TabsTrigger value="upload">
              <FileArchive className="size-3.5" />
              {t('packs:import.tabUpload', { defaultValue: '上传' })}
            </TabsTrigger>
          </TabsList>

          <TabsContent value="local" className="mt-4 space-y-3">
            <Input
              value={localPath}
              onChange={(e) => setLocalPath(e.target.value)}
              placeholder={t('packs:import.localPlaceholder', {
                defaultValue: '服务器上 .epzip 文件的绝对路径',
              })}
              className="font-mono text-xs"
              disabled={submitting}
            />
            <div className="flex justify-end">
              <Button
                disabled={!localPath.trim() || submitting}
                onClick={() => void submitLocal()}
              >
                {submitting ? <Loader2 className="size-4 animate-spin" /> : null}
                {t('common:action.import')}
              </Button>
            </div>
          </TabsContent>

          <TabsContent value="url" className="mt-4 space-y-3">
            <Input
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://github.com/.../release/download/v1.0.0/pack.epzip"
              className="font-mono text-xs"
              disabled={submitting}
            />
            <p className="text-[11px] text-muted-foreground">
              {t('packs:import.urlHint', {
                defaultValue: '后端先下载归档再受理，大文件等待时间较长',
              })}
            </p>
            <div className="flex justify-end">
              <Button
                disabled={!url.trim() || submitting}
                onClick={() => void submitUrl()}
              >
                {submitting ? <Loader2 className="size-4 animate-spin" /> : null}
                {t('common:action.import')}
              </Button>
            </div>
          </TabsContent>

          <TabsContent value="upload" className="mt-4 space-y-3">
            {pickedFile ? (
              <div className="flex items-center justify-between gap-3 rounded-md border border-border bg-muted/20 px-3 py-2.5">
                <div className="flex min-w-0 items-center gap-2">
                  <FileArchive className="size-4 shrink-0 text-muted-foreground" />
                  <span className="truncate text-sm">{pickedFile.name}</span>
                  <span className="shrink-0 font-mono text-xs text-muted-foreground">
                    {formatBytes(pickedFile.size)}
                  </span>
                </div>
                <Button
                  variant="ghost"
                  size="icon-xs"
                  disabled={submitting}
                  onClick={() => setPickedFile(null)}
                  aria-label={t('common:action.close')}
                >
                  <X className="size-3.5" />
                </Button>
              </div>
            ) : (
              <Button
                variant="outline"
                className="w-full border-dashed"
                onClick={() => fileInputRef.current?.click()}
                disabled={submitting}
              >
                <FileArchive className="size-4" />
                {t('packs:import.pickFile', { defaultValue: '选择 .epzip 文件' })}
              </Button>
            )}
            <input
              ref={fileInputRef}
              type="file"
              accept=".epzip,.zip"
              className="hidden"
              onChange={(e) => {
                const file = e.target.files?.[0]
                if (file) setPickedFile(file)
                e.target.value = ''
              }}
            />
            {uploadProgress !== null && (
              <div className="space-y-1">
                <div className="flex justify-between font-mono text-[11px] text-muted-foreground">
                  <span>{t('packs:import.uploading', { defaultValue: '正在上传' })}</span>
                  <span>{Math.floor(uploadProgress.percent)}%</span>
                </div>
                <Progress value={uploadProgress.percent} className="h-1.5" />
              </div>
            )}
            <div className="flex items-center justify-end gap-2">
              {submitting && (
                <Button variant="ghost" size="sm" onClick={() => abortRef.current?.()}>
                  {t('common:action.cancel')}
                </Button>
              )}
              <Button
                disabled={!pickedFile || submitting}
                onClick={() => void submitUpload()}
              >
                {submitting ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <Upload className="size-4" />
                )}
                {t('importModule.startUpload', { defaultValue: '上传并导入' })}
              </Button>
            </div>
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  )
}

// ─── 导出模块对话框（模块+变体勾选 / 许可证模式 / 管线 / 包身份）────────────

/** 可导出变体候选：已就绪且带全限定 ID */
interface ExportVariantCandidate {
  model: ModelInfo
  /** `<qualified_id>@<variant>` */
  pin: string
}

interface ExportModuleGroup {
  moduleId: string
  moduleName: string
  variants: ExportVariantCandidate[]
}

function ExportModuleDialog({
  open,
  onClose,
  models,
  io,
}: {
  open: boolean
  onClose: () => void
  models: ModelListResponse | null
  io: UsePackIoResult
}) {
  const { t } = useTranslation('modules')
  const [pipelines, setPipelines] = useState<PipelineSummary[]>([])
  const [loadingPipelines, setLoadingPipelines] = useState(false)
  const [selectedPins, setSelectedPins] = useState<Set<string>>(new Set())
  /** 许可证模式 = bundle 的模块集合（其余为 reference） */
  const [bundleModules, setBundleModules] = useState<Set<string>>(new Set())
  const [selectedPipelines, setSelectedPipelines] = useState<Set<string>>(new Set())
  const [packId, setPackId] = useState('')
  const [packName, setPackName] = useState('')
  const [packVersion, setPackVersion] = useState('')
  const [packDescription, setPackDescription] = useState('')
  const [submitting, setSubmitting] = useState(false)

  // 打开时复位表单并拉取管线列表
  useEffect(() => {
    if (!open) return
    setSelectedPins(new Set())
    setBundleModules(new Set())
    setSelectedPipelines(new Set())
    setPackId('')
    setPackName('')
    setPackVersion('')
    setPackDescription('')
    setLoadingPipelines(true)
    api
      .listPipelines()
      .then(setPipelines)
      .catch(() => setPipelines([]))
      .finally(() => setLoadingPipelines(false))
  }, [open])

  /** 候选分组：按模块聚合已就绪且带 qualified_id 的变体 */
  const groups = useMemo<ExportModuleGroup[]>(() => {
    const out: ExportModuleGroup[] = []
    for (const group of models?.modules ?? []) {
      const variants: ExportVariantCandidate[] = []
      for (const model of group.models) {
        if (!isReadyStatus(model.status) || !model.qualified_id) continue
        variants.push({
          model,
          pin: `${model.qualified_id}@${model.model_id}`,
        })
      }
      if (variants.length > 0) {
        out.push({
          moduleId: group.module_id,
          moduleName: group.module_name,
          variants,
        })
      }
    }
    return out
  }, [models])

  function togglePin(pin: string) {
    setSelectedPins((prev) => {
      const next = new Set(prev)
      if (next.has(pin)) next.delete(pin)
      else next.add(pin)
      return next
    })
  }

  function toggleModuleAll(group: ExportModuleGroup) {
    const allSelected = group.variants.every((v) => selectedPins.has(v.pin))
    setSelectedPins((prev) => {
      const next = new Set(prev)
      for (const v of group.variants) {
        if (allSelected) next.delete(v.pin)
        else next.add(v.pin)
      }
      return next
    })
  }

  function setBundleMode(moduleId: string, bundle: boolean) {
    setBundleModules((prev) => {
      const next = new Set(prev)
      if (bundle) next.add(moduleId)
      else next.delete(moduleId)
      return next
    })
  }

  function togglePipeline(id: string) {
    setSelectedPipelines((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  async function handleBuild() {
    if (submitting) return
    const id = packId.trim()
    if (id && !PACK_ID_PATTERN.test(id)) {
      toast.error(t('packs:build.invalidId', { defaultValue: '包 ID 格式无效' }), {
        description: t('packs:build.invalidIdHint', {
          defaultValue: '需为 <publisher>.<pack-name>，仅小写字母/数字/连字符',
        }),
      })
      return
    }
    setSubmitting(true)
    try {
      // bundle 列表：bundle 模式下被勾选模块的 qualified_id（去重）
      const pinModule = new Map<string, string>()
      for (const g of groups) {
        for (const v of g.variants) pinModule.set(v.pin, g.moduleId)
      }
      const bundleIds = new Set<string>()
      for (const pin of selectedPins) {
        const moduleId = pinModule.get(pin)
        if (moduleId && bundleModules.has(moduleId)) {
          bundleIds.add(pin.split('@')[0])
        }
      }
      const resp = await io.build(
        {
          models: [...selectedPins],
          pipelines: [...selectedPipelines],
          bundle: [...bundleIds],
          ...(id ? { id } : {}),
          ...(packName.trim() ? { name: packName.trim() } : {}),
          ...(packVersion.trim() ? { version: packVersion.trim() } : {}),
          ...(packDescription.trim() ? { description: packDescription.trim() } : {}),
        },
        { autoDownload: true },
      )
      toast.success(t('exportModule.accepted', { defaultValue: '模块导出构建已受理' }), {
        description: t('exportModule.acceptedDesc', {
          defaultValue: '构建完成后将自动开始下载（{{id}}）',
          id: resp.pack_id,
        }),
      })
      onClose()
    } catch (e) {
      toast.error(t('packs:toast.buildFailed', { defaultValue: '构建受理失败' }), {
        description: errMsg(e),
      })
    } finally {
      setSubmitting(false)
    }
  }

  const nothingSelected = selectedPins.size === 0 && selectedPipelines.size === 0
  const idInvalid = packId.trim() !== '' && !PACK_ID_PATTERN.test(packId.trim())

  return (
    <Dialog open={open} onOpenChange={(v) => (v ? undefined : onClose())}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <PackagePlus className="size-4 text-primary" />
            {t('toolbar.export', { defaultValue: '导出模块' })}
          </DialogTitle>
          <DialogDescription>
            {t('exportModule.description', {
              defaultValue:
                '勾选模块（含变体）与管线；构建完成后自动下载 .epzip 分发',
            })}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-5">
          {/* 模块（含变体）勾选 + 每模块许可证模式 */}
          <div className="space-y-2">
            <p className="text-sm font-medium">
              {t('exportModule.modules', { defaultValue: '选择模块（含变体）' })}
              <span className="ml-2 font-mono text-xs text-muted-foreground">
                {selectedPins.size}
              </span>
            </p>
            {groups.length === 0 ? (
              <p className="rounded-md border border-dashed border-border px-3 py-4 text-center text-xs text-muted-foreground">
                {t('exportModule.noCandidates', {
                  defaultValue:
                    '无可导出模块：仅已就绪且带全限定 ID 的变体可导出（整合包导入的模型或已设置标签后下载的模型）',
                })}
              </p>
            ) : (
              <div className="max-h-64 space-y-2 overflow-y-auto pr-1">
                {groups.map((g) => {
                  const allSelected =
                    g.variants.length > 0 &&
                    g.variants.every((v) => selectedPins.has(v.pin))
                  const isBundle = bundleModules.has(g.moduleId)
                  return (
                    <div key={g.moduleId} className="rounded-lg border border-border">
                      <div className="flex items-center gap-2.5 border-b border-border/60 px-3 py-2">
                        <input
                          type="checkbox"
                          className="size-4 shrink-0 accent-primary"
                          checked={allSelected}
                          onChange={() => toggleModuleAll(g)}
                          aria-label={g.moduleName}
                        />
                        <span className="min-w-0 flex-1 truncate text-sm font-medium">
                          {g.moduleName}
                        </span>
                        <span className="shrink-0 font-mono text-[10px] text-muted-foreground">
                          {g.variants.filter((v) => selectedPins.has(v.pin)).length}/
                          {g.variants.length}
                        </span>
                      </div>
                      <div className="space-y-2 px-3 py-2">
                        {/* 许可证模式（每模块二选一） */}
                        <div className="space-y-1">
                          <p className="text-[11px] text-muted-foreground">
                            {t('exportModule.licenseHint', {
                              defaultValue: '按许可证选择分发方式',
                            })}
                          </p>
                          <div className="flex flex-wrap gap-x-4 gap-y-1">
                            <label className="flex cursor-pointer items-center gap-1.5 text-xs">
                              <input
                                type="radio"
                                name={`ep-license-${g.moduleId}`}
                                className="accent-primary"
                                checked={!isBundle}
                                onChange={() => setBundleMode(g.moduleId, false)}
                              />
                              <span
                                title={t('exportModule.modeReferenceHint', {
                                  defaultValue:
                                    '包体小；导入时按模型声明的来源渠道自动下载权重',
                                })}
                              >
                                {t('exportModule.modeReference', {
                                  defaultValue: '仅元数据，从指定渠道下载',
                                })}
                              </span>
                            </label>
                            <label className="flex cursor-pointer items-center gap-1.5 text-xs">
                              <input
                                type="radio"
                                name={`ep-license-${g.moduleId}`}
                                className="accent-primary"
                                checked={isBundle}
                                onChange={() => setBundleMode(g.moduleId, true)}
                              />
                              <span
                                title={t('exportModule.modeBundleHint', {
                                  defaultValue: '权重随包分发，包体大但可离线导入',
                                })}
                              >
                                {t('exportModule.modeBundle', {
                                  defaultValue: '随包附带权重文件',
                                })}
                              </span>
                            </label>
                          </div>
                        </div>
                        {/* 变体勾选 */}
                        <div className="space-y-0.5">
                          {g.variants.map((v) => (
                            <label
                              key={v.pin}
                              className="flex cursor-pointer items-center gap-2.5 rounded px-1 py-1 transition-colors hover:bg-muted/40"
                            >
                              <input
                                type="checkbox"
                                className="size-4 shrink-0 accent-primary"
                                checked={selectedPins.has(v.pin)}
                                onChange={() => togglePin(v.pin)}
                              />
                              <span className="min-w-0 flex-1 truncate font-mono text-xs">
                                {v.pin}
                              </span>
                              <span className="shrink-0 font-mono text-[10px] text-muted-foreground">
                                {formatMB(v.model.size_estimate_mb)}
                              </span>
                            </label>
                          ))}
                        </div>
                      </div>
                    </div>
                  )
                })}
              </div>
            )}
          </div>

          {/* 管线选择 */}
          <div className="space-y-2">
            <p className="text-sm font-medium">
              {t('packs:build.pipelines', { defaultValue: '附带管线（可选）' })}
            </p>
            {loadingPipelines ? (
              <Skeleton className="h-9 rounded-md" />
            ) : pipelines.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {t('packs:build.noPipelines', { defaultValue: '暂无已注册管线' })}
              </p>
            ) : (
              <div className="max-h-40 divide-y divide-border overflow-y-auto rounded-lg border border-border">
                {pipelines.map((p) => (
                  <label
                    key={p.id}
                    className="flex cursor-pointer items-center gap-2.5 px-3 py-2"
                  >
                    <input
                      type="checkbox"
                      className="size-4 shrink-0 accent-primary"
                      checked={selectedPipelines.has(p.id)}
                      onChange={() => togglePipeline(p.id)}
                      aria-label={p.id}
                    />
                    <span className="min-w-0 flex-1 truncate text-xs">
                      {p.name}
                      <span className="ml-2 font-mono text-[10px] text-muted-foreground">
                        {p.id}
                      </span>
                    </span>
                  </label>
                ))}
              </div>
            )}
          </div>

          {/* 包身份（可选，缺省自动生成） */}
          <div className="space-y-2 rounded-lg border border-border p-3">
            <p className="text-sm font-medium">
              {t('packs:build.identity', { defaultValue: '包身份（可选）' })}
            </p>
            <div className="grid gap-2 sm:grid-cols-2">
              <div className="space-y-1 sm:col-span-2">
                <Input
                  value={packId}
                  onChange={(e) => setPackId(e.target.value)}
                  placeholder={t('packs:build.idPlaceholder', {
                    defaultValue: 'publisher.pack-name（留空自动生成）',
                  })}
                  className={cn('font-mono text-xs', idInvalid && 'border-status-error')}
                />
                {idInvalid && (
                  <p className="text-[11px] text-status-error">
                    {t('packs:build.invalidIdHint', {
                      defaultValue: '需为 <publisher>.<pack-name>，仅小写字母/数字/连字符',
                    })}
                  </p>
                )}
              </div>
              <Input
                value={packName}
                onChange={(e) => setPackName(e.target.value)}
                placeholder={t('packs:build.namePlaceholder', {
                  defaultValue: '显示名称（可选）',
                })}
                className="text-xs"
              />
              <Input
                value={packVersion}
                onChange={(e) => setPackVersion(e.target.value)}
                placeholder={t('packs:build.versionPlaceholder', {
                  defaultValue: '版本号，如 1.0.0（可选）',
                })}
                className="font-mono text-xs"
              />
              <Input
                value={packDescription}
                onChange={(e) => setPackDescription(e.target.value)}
                placeholder={t('packs:build.descriptionPlaceholder', {
                  defaultValue: '描述（可选）',
                })}
                className="text-xs sm:col-span-2"
              />
            </div>
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={submitting}>
            {t('common:action.cancel')}
          </Button>
          <Button
            disabled={nothingSelected || submitting || idInvalid}
            onClick={() => void handleBuild()}
          >
            {submitting ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <PackagePlus className="size-4" />
            )}
            {t('exportModule.submit', { defaultValue: '开始导出' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ─── 模型级上传 / 本地路径导入对话框（MODULE_SPEC §6.3）──────────────

/**
 * 浏览器上传模型文件（§6.3）或导入服务器已有目录。
 * - 上传：文件夹多文件（webkitdirectory）或单个 .zip/.tar.gz/.tgz 归档；
 *   本地网络场景不做尺寸限制（后端 DefaultBodyLimit 已禁用）。
 * - 导入：服务器本地路径 → 写入 .ep_meta.json，来源标记后可检查更新。
 */
function ModelUploadDialog({
  open,
  moduleId,
  moduleName,
  modelId,
  modelName,
  onClose,
  onSettled,
}: {
  open: boolean
  moduleId: string
  moduleName: string
  modelId: string
  modelName: string
  onClose: () => void
  onSettled: () => void
}) {
  const { t } = useTranslation('modules')
  const [tab, setTab] = useState<'upload' | 'import'>('upload')
  const [files, setFiles] = useState<File[]>([])
  const [sourcePath, setSourcePath] = useState('')
  const [submitting, setSubmitting] = useState(false)
  /** 上传进度百分比（null = 未在传输；XHR 不可计算时不更新） */
  const [uploadPercent, setUploadPercent] = useState<number | null>(null)

  useEffect(() => {
    if (!open) {
      setFiles([])
      setSourcePath('')
      setSubmitting(false)
      setUploadPercent(null)
    }
  }, [open])

  const totalBytes = files.reduce((acc, f) => acc + f.size, 0)
  const archivePicked =
    files.length === 1 &&
    /\.(zip|tar\.gz|tgz)$/i.test(files[0].name ?? '')

  async function submitUpload() {
    if (files.length === 0 || submitting) return
    setSubmitting(true)
    setUploadPercent(0)
    try {
      if (archivePicked) {
        await uploadModelWithProgress(moduleId, modelId, files, undefined, (p) =>
          setUploadPercent(p.percent),
        )
      } else {
        const paths = files.map((f) =>
          (f as File & { webkitRelativePath?: string }).webkitRelativePath ||
          f.name,
        )
        await uploadModelWithProgress(moduleId, modelId, files, paths, (p) =>
          setUploadPercent(p.percent),
        )
      }
      toast.success(t('modelUpload.succeeded', { defaultValue: '模型文件已上传' }), {
        description: `${moduleName} / ${modelName}`,
      })
      onSettled()
      onClose()
    } catch (e) {
      toast.error(t('modelUpload.failed', { defaultValue: '模型上传失败' }), {
        description: errMsg(e),
      })
    } finally {
      setSubmitting(false)
      setUploadPercent(null)
    }
  }

  async function submitImport() {
    if (!sourcePath.trim() || submitting) return
    setSubmitting(true)
    try {
      const resp = await api.importModel(moduleId, {
        model_id: modelId,
        source_path: sourcePath.trim(),
      })
      if (resp.error) throw new Error(resp.error)
      toast.success(t('modelUpload.importSucceeded', { defaultValue: '模型已从本地路径导入' }), {
        description: `${moduleName} / ${modelName}`,
      })
      onSettled()
      onClose()
    } catch (e) {
      toast.error(t('modelUpload.importFailed', { defaultValue: '导入失败' }), {
        description: errMsg(e),
      })
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={(v) => (v ? undefined : onClose())}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Upload className="size-4 text-primary" />
            {t('modelUpload.title', {
              defaultValue: '上传 / 导入模型「{{name}}」',
              name: modelName,
            })}
          </DialogTitle>
          <DialogDescription>
            {t('modelUpload.description', {
              defaultValue:
                '浏览器上传文件夹或压缩包（zip/tar.gz），或导入服务器上已存在的目录；本地网络不做尺寸限制',
            })}
          </DialogDescription>
        </DialogHeader>

        <Tabs value={tab} onValueChange={(v) => setTab(v as 'upload' | 'import')}>
          <TabsList>
            <TabsTrigger value="upload">
              <Upload className="size-3.5" />
              {t('modelUpload.tabUpload', { defaultValue: '浏览器上传' })}
            </TabsTrigger>
            <TabsTrigger value="import">
              <FolderOpen className="size-3.5" />
              {t('modelUpload.tabImport', { defaultValue: '本地路径导入' })}
            </TabsTrigger>
          </TabsList>

          <TabsContent value="upload" className="space-y-3">
            <label className="flex cursor-pointer flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border px-4 py-8 text-center">
              <FolderOpen className="size-6 text-muted-foreground" />
              <span className="text-sm text-muted-foreground">
                {t('modelUpload.choose', {
                  defaultValue: '选择文件夹（逐文件上传）或单个压缩包',
                })}
              </span>
              <input
                type="file"
                multiple
                className="hidden"
                onChange={(e) => setFiles(Array.from(e.target.files ?? []))}
              />
            </label>
            {files.length > 0 ? (
              <div className="space-y-1.5 rounded-lg border border-border bg-muted/20 p-3">
                <div className="flex items-center justify-between text-xs">
                  <span className="font-medium">
                    {files.length}{' '}
                    {t('modelUpload.fileCount', {
                      defaultValue: '个文件',
                      count: files.length,
                    })}
                  </span>
                  <span className="font-mono text-muted-foreground">
                    {formatBytes(totalBytes)}
                  </span>
                </div>
                <ul className="max-h-40 space-y-0.5 overflow-y-auto text-xs text-muted-foreground">
                  {files.slice(0, 50).map((f, i) => (
                    <li key={i} className="truncate font-mono">
                      {(f as File & { webkitRelativePath?: string })
                        .webkitRelativePath || f.name}
                    </li>
                  ))}
                  {files.length > 50 ? (
                    <li>
                      … {t('modelUpload.more', { defaultValue: '更多' })} (
                      {files.length - 50})
                    </li>
                  ) : null}
                </ul>
              </div>
            ) : null}
          </TabsContent>

          <TabsContent value="import" className="space-y-3">
            <Input
              value={sourcePath}
              onChange={(e) => setSourcePath(e.target.value)}
              placeholder={t('modelUpload.importPlaceholder', {
                defaultValue: '服务器上已存在的模型目录路径',
              })}
              className="font-mono text-xs"
            />
            <p className="text-xs text-muted-foreground">
              {t('modelUpload.importHint', {
                defaultValue:
                  '导入目录将被识别为模型来源并写入元数据，之后可检查更新',
              })}
            </p>
          </TabsContent>
        </Tabs>

        {uploadPercent !== null && (
          <div className="flex items-center gap-2">
            <Progress value={uploadPercent} className="h-1.5" />
            <span className="shrink-0 font-mono text-xs text-muted-foreground">
              {Math.floor(uploadPercent)}%
            </span>
          </div>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={submitting}>
            {t('common:action.cancel')}
          </Button>
          <Button
            disabled={
              submitting ||
              (tab === 'upload' ? files.length === 0 : !sourcePath.trim())
            }
            onClick={() => void (tab === 'upload' ? submitUpload() : submitImport())}
          >
            {submitting ? <Loader2 className="size-4 animate-spin" /> : <Upload className="size-4" />}
            {tab === 'upload'
              ? t('modelUpload.submitUpload', { defaultValue: '上传' })
              : t('modelUpload.submitImport', { defaultValue: '导入' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ─── 页面 ────────────────────────────────────────────────────────────────────

/**
 * 模块管理统一页（信息架构 #47：模型 = 模块）。
 *
 * - 每模块一张卡（用户视角的「模型」）：变体选择器折叠全部变体，
 *   不将变体渲染为独立模型行；
 * - 顶部工具栏：导入模块（整合包导入三来源）/ 导出模块（构建 + 许可证模式）；
 * - pack 来源模块卡带「整合包来源」徽章，徽章菜单提供卸载；
 * - 进度统一走 WS pack_import / model_download。
 */
export function ModulesPage() {
  const { t } = useTranslation('modules')
  // 数据层：模型分组（变体维度）+ 模块维度（运行状态 5s 轮询）
  const { models, refresh, loading, error } = useModels()
  const {
    modules,
    statusMap,
    loading: modulesLoading,
    error: modulesError,
    refresh: refreshModules,
  } = useModules()

  // ── 抽屉 / 对话框状态 ──
  const [logsModuleId, setLogsModuleId] = useState<string | null>(null)
  const [detailModule, setDetailModule] = useState<ModuleResponse | null>(null)
  const [runModule, setRunModule] = useState<ModuleResponse | null>(null)
  const [tagsTarget, setTagsTarget] = useState<TagsTarget | null>(null)
  const [stopTarget, setStopTarget] = useState<ModuleResponse | null>(null)
  const [importOpen, setImportOpen] = useState(false)
  const [exportOpen, setExportOpen] = useState(false)
  const [uninstallTarget, setUninstallTarget] = useState<string | null>(null)
  const [keepModels, setKeepModels] = useState(true)
  const [uninstalling, setUninstalling] = useState(false)
  // ── 模型级操作（§6.3 / §5.1 卡内删除）：上传/导入、检查更新、删除 ──
  const [uploadTarget, setUploadTarget] = useState<{
    moduleId: string
    moduleName: string
    model: ModelInfo
  } | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<{
    moduleId: string
    moduleName: string
    model: ModelInfo
  } | null>(null)
  const [deleting, setDeleting] = useState(false)
  /** 列表级 tag 筛选（§5.1 chips 筛选；null = 不过滤） */
  const [tagFilter, setTagFilter] = useState<string | null>(null)

  /** 全部数据源刷新（模块 + 模型；pack 导入/卸载后落位可能变化） */
  async function refreshAll() {
    await Promise.allSettled([refresh(), refreshModules()])
  }

  // ── 模型下载状态：WS 推送为主 + 轮询兜底 ──
  function handleDownloadSettled(entry: ModelDownloadProgress) {
    void refresh()
    const group = models?.modules.find((g) => g.module_id === entry.module_id)
    const name =
      group?.models.find((m) => m.model_id === entry.model_id)?.name ??
      entry.model_id
    if (entry.state === 'completed') {
      toast.success(t('models:download.completed'), { description: name })
    } else if (entry.state === 'failed') {
      toast.error(t('models:download.failed'), {
        description: t('models:download.failedDesc', { name }),
      })
    } else {
      toast.info(t('models:download.cancelled'), { description: name })
    }
  }

  const { get: getDownload, refresh: refreshDownloads } =
    useModelDownloads(handleDownloadSettled)

  // ── 整合包 I/O（导入/导出模块工具栏 + 卸载）──
  const packIo = usePackIo({ onSettled: handlePackSettled })

  function handlePackSettled(packId: string, entry: PackProgressEntry) {
    const isBuild = entry.stage === 'build'
    if (entry.state === 'completed') {
      toast.success(
        isBuild
          ? t('pack.buildCompleted', {
              defaultValue: '模块导出构建完成，已开始下载',
            })
          : t('pack.importCompleted', {
              defaultValue: '导入完成：模块 + 模型 + 管线已落位',
            }),
        { description: entry.message ?? packId },
      )
      // 导入完成后可能新增模块/模型/管线；卸载同理需要刷新
      void refreshAll()
    } else if (entry.state === 'failed') {
      toast.error(t('pack.failed', { defaultValue: '整合包操作失败' }), {
        description: entry.message ?? packId,
      })
    }
  }

  // ── 模块操作 ──
  async function startModule(m: ModuleResponse) {
    try {
      const res = await api.startModule(m.id)
      if (res.error) throw new Error(res.error)
      toast.success(t('models:module.startStarted', { defaultValue: '模块启动中' }), {
        description: m.name,
      })
      await refreshModules()
    } catch (e) {
      toast.error(t('models:module.startFailed', { defaultValue: '模块启动失败' }), {
        description: errMsg(e),
      })
    }
  }

  async function stopModuleConfirmed() {
    if (!stopTarget) return
    try {
      const res = await api.stopModule(stopTarget.id)
      if (res.error) throw new Error(res.error)
      toast.success(
        t('models:module.stopSucceeded', { defaultValue: '模块已停止' }),
        { description: stopTarget.name },
      )
      await refreshModules()
    } catch (e) {
      toast.error(t('models:module.stopFailed', { defaultValue: '模块停止失败' }), {
        description: errMsg(e),
      })
      throw e // 保持确认框打开，便于重试
    }
  }

  /** 重启模块（先停后启；用于变体切换生效引导） */
  async function restartModule(m: ModuleResponse) {
    try {
      await api.stopModule(m.id)
    } catch {
      // 模块可能已停止：忽略停止失败继续尝试启动
    }
    await startModule(m)
  }

  // ── 变体单槽位切换（PUT /api/models/{m}/{mid}/variant）──
  async function switchVariant(m: ModuleResponse, modelId: string) {
    const group = groupByModule.get(m.id)
    const current = resolveActiveVariant(m, group?.models ?? [])
    if (current?.model_id === modelId) return
    try {
      const resp = await api.setModelVariant(m.id, modelId, { model_id: modelId })
      await Promise.allSettled([refresh(), refreshModules()])
      if (resp.needs_download) {
        const model = group?.models.find((x) => x.model_id === modelId)
        toast.warning(
          t('models:variant.needsDownload', {
            defaultValue: '该变体本地缺失，需要下载',
          }),
          {
            description: modelId,
            action: model
              ? {
                  label: t('models:variant.downloadNow', { defaultValue: '立即下载' }),
                  onClick: () => void startDownload(m.id, model),
                }
              : undefined,
          },
        )
      } else if (resp.needs_restart) {
        toast.info(
          t('models:variant.needsRestart', {
            defaultValue: '变体已切换，重启模块后生效',
          }),
          {
            description: m.name,
            action: {
              label: t('models:variant.restartNow', { defaultValue: '立即重启' }),
              onClick: () => void restartModule(m),
            },
          },
        )
      } else {
        toast.success(
          t('models:variant.switchSuccess', { defaultValue: '激活变体已切换' }),
          { description: modelId },
        )
      }
    } catch (e) {
      toast.error(
        t('models:variant.switchFailed', { defaultValue: '变体切换失败' }),
        { description: errMsg(e) },
      )
    }
  }

  // ── 下载（选中变体）──
  async function startDownload(
    moduleId: string,
    model: ModelInfo,
    source?: ModelSource,
  ) {
    const chosen = source ?? primarySource(model)
    try {
      await api.downloadModel(moduleId, {
        model_id: model.model_id,
        source: chosen,
      })
      toast.success(t('models:download.started'), {
        description: chosen
          ? `${model.name} · ${sourceLabel(t, chosen)}`
          : model.name,
      })
      // 立即拉一次下载列表，保证进度条尽快出现（WS 初始推送可能稍晚）
      void refreshDownloads()
    } catch (e) {
      toast.error(t('models:download.startFailed'), { description: errMsg(e) })
    }
  }

  async function cancelDownload(moduleId: string, model: ModelInfo) {
    try {
      await api.cancelModelDownload(moduleId, model.model_id)
      toast.info(
        t('models:download.cancelRequested', { defaultValue: '已请求取消下载' }),
        { description: model.name },
      )
      void refreshDownloads()
    } catch (e) {
      toast.error(
        t('models:download.cancelFailed', { defaultValue: '取消下载失败' }),
        { description: errMsg(e) },
      )
    }
  }

  // ── 检查更新（POST /api/models/{m}/{mid}/check-update；手动触发不受
  //    check_updates 开关约束，协调 #51 语义）──
  async function checkUpdate(moduleId: string, model: ModelInfo) {
    try {
      const resp = await api.checkModelUpdate(moduleId, model.model_id)
      if (resp.available) {
        toast.info(
          t('modelUpdate.available', { defaultValue: '发现可用更新' }),
          { description: resp.reason || model.name },
        )
      } else {
        toast.success(
          t('modelUpdate.upToDate', { defaultValue: '已是最新版本' }),
          { description: resp.reason || model.name },
        )
      }
      void refresh()
    } catch (e) {
      toast.error(
        t('modelUpdate.failed', { defaultValue: '检查更新失败' }),
        { description: errMsg(e) },
      )
    }
  }

  // ── 删除模型（§5.1 卡内删除；确认框在页面底部）──
  async function confirmDeleteModel() {
    if (!deleteTarget || deleting) return
    setDeleting(true)
    try {
      await api.deleteModel(deleteTarget.moduleId, deleteTarget.model.model_id)
      toast.success(
        t('modelDelete.succeeded', { defaultValue: '模型已删除' }),
        { description: deleteTarget.model.name },
      )
      setDeleteTarget(null)
      await refreshAll()
    } catch (e) {
      toast.error(
        t('modelDelete.failed', { defaultValue: '模型删除失败' }),
        { description: errMsg(e) },
      )
      throw e // 保持确认框打开便于重试
    } finally {
      setDeleting(false)
    }
  }

  // ── 卸载来源整合包（徽章菜单 → 确认框）──
  async function confirmUninstall() {
    if (!uninstallTarget || uninstalling) return
    setUninstalling(true)
    try {
      await packIo.uninstall(uninstallTarget, keepModels)
      toast.success(t('packs:toast.uninstalled', { defaultValue: '整合包已卸载' }), {
        description: uninstallTarget,
      })
      setUninstallTarget(null)
      await refreshAll()
    } catch (e) {
      toast.error(
        t('packs:toast.uninstallFailed', { defaultValue: '卸载失败' }),
        { description: errMsg(e) },
      )
    } finally {
      setUninstalling(false)
    }
  }

  /** module_id → 模型分组（仅含声明了模型的模块） */
  const groupByModule = useMemo(() => {
    const map = new Map<
      string,
      { module_id: string; module_name: string; models: ModelInfo[] }
    >()
    for (const group of models?.modules ?? []) {
      map.set(group.module_id, group)
    }
    return map
  }, [models])

  /** 全部模型的 tag 集合（列表级 chips 筛选，§5.1） */
  const allTags = useMemo(() => {
    const set = new Set<string>()
    for (const group of models?.modules ?? []) {
      for (const model of group.models) {
        for (const tag of model.tags ?? []) set.add(tag)
      }
    }
    return Array.from(set).sort()
  }, [models])

  /** tag 筛选后的模块列表（模块任一模型命中 tag 即保留） */
  const visibleModules = useMemo(() => {
    if (!tagFilter) return modules
    return modules.filter((m) => {
      const group = groupByModule.get(m.id)
      return (group?.models ?? []).some(
        (model) => model.tags?.includes(tagFilter as string),
      )
    })
  }, [modules, tagFilter, groupByModule])

  /**
   * 解析模块激活变体（单槽位）：后端 ModuleResponse.active_model_id 为
   * 权威数据源（config.active_models → default → 首变体已解析）；
   * 过渡期字段缺失时回退清单首项。
   */
  function resolveActiveVariant(
    m: ModuleResponse,
    modelList: ModelInfo[],
  ): ModelInfo | null {
    if (modelList.length === 0) return null
    const pinned = m.active_model_id
    if (pinned) {
      const found = modelList.find((x) => x.model_id === pinned)
      if (found) return found
    }
    return modelList[0]
  }

  const progressEntries = Object.entries(packIo.progress)
  const pageLoading = loading || modulesLoading
  const loadError = modulesError ?? error

  // ── 统一卡片：每模块一张（模型 = 模块）──
  function renderModuleCard(m: ModuleResponse) {
    const group = groupByModule.get(m.id)
    const svcStatus = statusMap[m.id]?.status ?? m.service_status
    const svcKey = normalizeStatus(svcStatus).replace(/\s+/g, '_')
    const canStart =
      svcKey === 'stopped' || svcKey === 'error' || svcKey === 'not_ready'
    const canStop = ['running', 'starting', 'preparing'].includes(svcKey)
    const modelList = group?.models ?? []
    const activeModel = resolveActiveVariant(m, modelList)
    const activeDownload = activeModel
      ? getDownload(m.id, activeModel.model_id)
      : undefined
    const activeDlState = activeDownload?.state
    const activeDlActive =
      activeDlState === 'downloading' || activeDlState === 'queued'
    // pack 来源徽章（按模型 pack_id 去重）
    const packIds: string[] = []
    for (const model of modelList) {
      if (model.pack_id && !packIds.includes(model.pack_id)) {
        packIds.push(model.pack_id)
      }
    }
    const capabilities = m.capabilities ?? []

    return (
      <Card key={m.id}>
        <CardHeader className="pb-3">
          <div className="flex items-center justify-between gap-2">
            <CardTitle className="flex min-w-0 flex-wrap items-center gap-2 text-base font-semibold">
              <Database className="size-4 shrink-0 text-primary" />
              <span className="truncate">{m.name}</span>
              {m.category ? (
                <Badge
                  variant="secondary"
                  className="px-1.5 text-[10px] font-normal text-muted-foreground"
                >
                  {categoryLabel(m.category)}
                </Badge>
              ) : null}
              {packIds.map((pid) => (
                <PackSourceBadge
                  key={pid}
                  packId={pid}
                  onUninstall={(packId) => {
                    setKeepModels(true)
                    setUninstallTarget(packId)
                  }}
                />
              ))}
            </CardTitle>
            <ServiceStatusBadge status={svcStatus} />
          </div>
          <CardDescription className="truncate font-mono text-xs">
            {m.id}
            {m.version ? ` · v${m.version}` : ''}
            {m.device ? ` · ${m.device}` : ''}
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          {/* 卡内操作：启动·停止·日志·详情·运行（直跑） */}
          <div className="flex flex-wrap items-center gap-1.5">
            {canStart ? (
              <Button size="xs" onClick={() => void startModule(m)}>
                <Play className="size-3" />
                {t('common:action.start')}
              </Button>
            ) : null}
            {canStop ? (
              <Button size="xs" variant="outline" onClick={() => setStopTarget(m)}>
                <Square className="size-3" />
                {t('common:action.stop')}
              </Button>
            ) : null}
            <Button size="xs" variant="ghost" onClick={() => setLogsModuleId(m.id)}>
              <ScrollText className="size-3" />
              {t('models:module.logs', { defaultValue: '日志' })}
            </Button>
            <Button size="xs" variant="ghost" onClick={() => setDetailModule(m)}>
              <SlidersHorizontal className="size-3" />
              {t('models:module.detail', { defaultValue: '详情' })}
            </Button>
            {capabilities.length > 0 ? (
              <Button size="xs" variant="secondary" onClick={() => setRunModule(m)}>
                <Zap className="size-3" />
                {t('models:module.run', { defaultValue: '运行' })}
              </Button>
            ) : null}
            {activeModel ? (
              <Button
                size="xs"
                variant="outline"
                onClick={() =>
                  setUploadTarget({
                    moduleId: m.id,
                    moduleName: m.name,
                    model: activeModel,
                  })
                }
              >
                <FolderOpen className="size-3" />
                {t('modelUpload.cardButton', { defaultValue: '上传/导入' })}
              </Button>
            ) : null}
          </div>

          {modelList.length > 0 ? (
            <div className="space-y-2">
              {/* 变体选择器：全部变体折叠为选项（不渲染独立模型行） */}
              <div className="flex flex-wrap items-center gap-2">
                <span className="shrink-0 text-xs text-muted-foreground">
                  {t('card.variant', { defaultValue: '变体' })}
                </span>
                <Select
                  value={activeModel?.model_id}
                  onValueChange={(v) => void switchVariant(m, v)}
                  disabled={modelList.length <= 1}
                >
                  <SelectTrigger className="h-7 w-auto min-w-40 max-w-full text-xs">
                    <SelectValue
                      placeholder={t('models:card.selectVariant', {
                        defaultValue: '选择变体',
                      })}
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {modelList.map((variant) => {
                      const vs = normalizeStatus(variant.status)
                      const statusText = t(`common:status.${vs}`, {
                        defaultValue: variant.status,
                      })
                      return (
                        <SelectItem key={variant.model_id} value={variant.model_id}>
                          {variant.name} · {statusText} ·{' '}
                          {formatMB(variant.size_estimate_mb)}
                          {variant.vram_estimate_mb
                            ? ` · VRAM ${formatMB(variant.vram_estimate_mb)}`
                            : ''}
                        </SelectItem>
                      )
                    })}
                  </SelectContent>
                </Select>
              </div>

              {/* 选中变体投影：状态 / 身份 / 体积 / tag / 下载 */}
              {activeModel ? (
                <div className="space-y-2 rounded-lg border border-primary/25 bg-primary/5 p-3">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="text-sm font-medium">{activeModel.name}</span>
                    <ModelStatusBadge
                      status={
                        activeDlState === 'downloading'
                          ? 'downloading'
                          : activeModel.status
                      }
                    />
                  </div>
                  <div className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-xs text-muted-foreground">
                    {activeModel.qualified_id ? (
                      <span className="break-all">{activeModel.qualified_id}</span>
                    ) : (
                      <span className="break-all">{activeModel.model_id}</span>
                    )}
                    {activeModel.vram_estimate_mb ? (
                      <span
                        className="flex items-center gap-1"
                        title={t('models:card.vramEstimate', {
                          defaultValue: 'VRAM 估算',
                        })}
                      >
                        <Gauge className="size-3" />
                        {formatMB(activeModel.vram_estimate_mb)}
                      </span>
                    ) : null}
                    {activeModel.size_estimate_mb ? (
                      <span className="flex items-center gap-1">
                        <MemoryStick className="size-3" />
                        {formatMB(activeModel.size_estimate_mb)}
                      </span>
                    ) : null}
                  </div>

                  {/* tag chips + 编辑 */}
                  <div className="flex flex-wrap items-center gap-1.5">
                    {(activeModel.tags ?? []).map((tag) => (
                      <Badge key={tag} variant="secondary" className="gap-1 pr-1">
                        <Tag className="size-2.5" />
                        {tag}
                      </Badge>
                    ))}
                    <Button
                      size="xs"
                      variant="ghost"
                      className="h-5 px-1.5 text-[10px] text-muted-foreground"
                      onClick={() => setTagsTarget({ moduleId: m.id, model: activeModel })}
                    >
                      <Pencil className="size-2.5" />
                      {(activeModel.tags ?? []).length === 0
                        ? t('models:tags.addFirst', { defaultValue: '添加标签' })
                        : t('common:action.edit')}
                    </Button>
                  </div>

                  {/* 模型级操作：检查更新 / 删除（§5.1 卡内删除；删除确认在页面底部） */}
                  <div className="flex flex-wrap items-center gap-1.5">
                    <Button
                      size="xs"
                      variant="ghost"
                      onClick={() => void checkUpdate(m.id, activeModel)}
                    >
                      <RefreshCw className="size-3" />
                      {t('modelUpdate.button', { defaultValue: '检查更新' })}
                    </Button>
                    <Button
                      size="xs"
                      variant="ghost"
                      className="text-destructive hover:text-destructive"
                      onClick={() =>
                        setDeleteTarget({
                          moduleId: m.id,
                          moduleName: m.name,
                          model: activeModel,
                        })
                      }
                    >
                      <Trash2 className="size-3" />
                      {t('common:action.delete')}
                    </Button>
                  </div>

                  {/* 选中变体下载进度（queued/downloading）+ 缺失时的下载入口 */}
                  {activeDlActive && activeDownload ? (
                    <div className="space-y-1.5">
                      <div className="flex items-center justify-between font-mono text-[11px] text-muted-foreground">
                        {activeDlState === 'queued' ? (
                          <span className="animate-pulse">
                            {t('common:status.queued', { defaultValue: '排队中' })}
                          </span>
                        ) : (
                          <span>{Math.floor(activeDownload.percent)}%</span>
                        )}
                        <span className="flex items-center gap-2">
                          {formatBytes(activeDownload.bytes)}
                          <button
                            type="button"
                            className="rounded px-1 text-foreground/70 underline-offset-2 hover:underline"
                            onClick={() => void cancelDownload(m.id, activeModel)}
                          >
                            {t('common:action.cancel')}
                          </button>
                        </span>
                      </div>
                      <Progress
                        value={activeDlState === 'queued' ? 0 : activeDownload.percent}
                        className={cn(
                          'h-1.5',
                          activeDlState === 'queued' && 'animate-pulse',
                        )}
                      />
                    </div>
                  ) : !isReadyStatus(activeModel.status) ? (
                    <div className="flex flex-wrap items-center gap-2">
                      {(activeModel.available_sources ?? []).length > 1 ? (
                        <DropdownMenu>
                          <DropdownMenuTrigger asChild>
                            <Button size="xs">
                              <Download className="size-3" />
                              {t('card.downloadVariant', {
                                defaultValue: '下载选中变体',
                              })}
                              <ChevronDown className="size-3" />
                            </Button>
                          </DropdownMenuTrigger>
                          <DropdownMenuContent align="end">
                            {(activeModel.available_sources ?? []).map((s) => (
                              <DropdownMenuItem
                                key={s}
                                onSelect={() => void startDownload(m.id, activeModel, s)}
                              >
                                <Download className="size-3.5" />
                                {sourceLabel(t, s)}
                              </DropdownMenuItem>
                            ))}
                          </DropdownMenuContent>
                        </DropdownMenu>
                      ) : (
                        <Button
                          size="xs"
                          onClick={() =>
                            void startDownload(
                              m.id,
                              activeModel,
                              (activeModel.available_sources ?? [])[0],
                            )
                          }
                        >
                          <Download className="size-3" />
                          {t('card.downloadVariant', { defaultValue: '下载选中变体' })}
                        </Button>
                      )}
                      <span className="text-[11px] text-muted-foreground">
                        {t('models:card.activeMissingHint', {
                          defaultValue: '激活变体未就绪，下载完成后方可启动模块',
                        })}
                      </span>
                    </div>
                  ) : null}
                </div>
              ) : null}
            </div>
          ) : (
            /* 无模型 native 模块：服务卡兜底 */
            <p className="rounded-lg border border-dashed border-border px-3 py-2.5 text-xs text-muted-foreground">
              {t('models:card.serviceFallback', {
                defaultValue:
                  '服务型模块（无模型声明）：可启动/停止、查看日志与能力详情',
              })}
            </p>
          )}
        </CardContent>
      </Card>
    )
  }

  return (
    <PageContainer
      title={t('page.title')}
      description={t('page.descriptionV2', {
        defaultValue:
          '模型即模块：卡片内完成变体选择、下载、启停与直跑；顶部工具栏导入 / 导出模块',
      })}
      actions={
        <>
          <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
            <Upload className="size-3.5" />
            {t('toolbar.import', { defaultValue: '导入模块' })}
          </Button>
          <Button size="sm" onClick={() => setExportOpen(true)}>
            <PackagePlus className="size-3.5" />
            {t('toolbar.export', { defaultValue: '导出模块' })}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void refresh()
              void refreshModules()
            }}
          >
            <RefreshCw className="size-3.5" />
            {t('common:action.refresh')}
          </Button>
        </>
      }
    >
      <div className="space-y-6">
        {loadError && (
          <div className="flex items-center gap-2 rounded-lg border border-status-error/30 bg-status-error/10 px-4 py-3 text-sm text-status-error">
            <TriangleAlert className="size-4 shrink-0" />
            <span className="min-w-0 flex-1 truncate">{errMsg(loadError)}</span>
            <Button
              variant="ghost"
              size="xs"
              onClick={() => {
                void refresh()
                void refreshModules()
              }}
            >
              {t('common:action.retry')}
            </Button>
          </div>
        )}

        {/* 导入 / 导出进度（WS pack_import 聚合） */}
        {progressEntries.length > 0 && (
          <div className="space-y-2">
            {progressEntries.map(([packId, entry]) => (
              <div
                key={packId}
                className="flex items-center gap-3 rounded-lg border border-border bg-muted/20 px-4 py-3"
              >
                {entry.state === 'running' ? (
                  <Loader2 className="size-4 shrink-0 animate-spin text-primary" />
                ) : entry.state === 'completed' ? (
                  <CircleCheck className="size-4 shrink-0 text-status-running" />
                ) : (
                  <CircleX className="size-4 shrink-0 text-status-error" />
                )}
                <div className="min-w-0 flex-1 space-y-1.5">
                  <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs">
                    <span className="font-mono font-medium">{packId}</span>
                    <span className="text-muted-foreground">
                      {entry.message ??
                        (entry.stage
                          ? t(`packs:stage.${entry.stage}`, {
                              defaultValue: entry.stage,
                            })
                          : '')}
                    </span>
                    {typeof entry.percent === 'number' &&
                    entry.state === 'running' ? (
                      <span className="font-mono text-muted-foreground">
                        {Math.floor(entry.percent)}%
                      </span>
                    ) : null}
                  </div>
                  {entry.state === 'running' ? (
                    <Progress
                      value={entry.percent ?? 0}
                      className={cn(
                        'h-1.5',
                        entry.percent === undefined && 'animate-pulse',
                      )}
                    />
                  ) : null}
                </div>
                {entry.state !== 'running' ? (
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    onClick={() => packIo.dismiss(packId)}
                    aria-label={t('common:action.close')}
                  >
                    <X className="size-3.5" />
                  </Button>
                ) : null}
              </div>
            ))}
          </div>
        )}

        {/* tag 列表级筛选 chips（§5.1） */}
        {allTags.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5">
            <Tag className="size-3.5 text-muted-foreground" />
            <Button
              size="xs"
              variant={tagFilter === null ? 'default' : 'outline'}
              onClick={() => setTagFilter(null)}
            >
              {t('modelFilter.all', { defaultValue: '全部' })}
            </Button>
            {allTags.map((tag) => (
              <Button
                key={tag}
                size="xs"
                variant={tagFilter === tag ? 'default' : 'outline'}
                onClick={() => setTagFilter(tagFilter === tag ? null : tag)}
              >
                {tag}
              </Button>
            ))}
          </div>
        )}

        {pageLoading ? (
          <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
            {Array.from({ length: 4 }).map((_, i) => (
              <Skeleton key={i} className="h-56 rounded-lg" />
            ))}
          </div>
        ) : modules.length === 0 ? (
          <Card>
            <EmptyState
              icon={Database}
              title={t('empty.title', { defaultValue: '暂无模块' })}
              description={t('page.emptyHint')}
              action={{
                label: t('common:action.refresh'),
                onClick: () => {
                  void refresh()
                  void refreshModules()
                },
              }}
            />
          </Card>
        ) : (
          <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
            {visibleModules.map((m) => renderModuleCard(m))}
          </div>
        )}
      </div>

      {/* ── 抽屉与对话框 ── */}
      <ModuleLogsSheet
        moduleId={logsModuleId}
        moduleName={
          modules.find((x) => x.id === logsModuleId)?.name ?? logsModuleId ?? ''
        }
        onClose={() => setLogsModuleId(null)}
      />
      <ModuleDetailSheet module={detailModule} onClose={() => setDetailModule(null)} />
      <DirectRunDrawer module={runModule} onClose={() => setRunModule(null)} />
      <TagEditorDialog
        target={tagsTarget}
        onClose={() => setTagsTarget(null)}
        onSaved={() => void refresh()}
      />
      <ImportModuleDialog
        open={importOpen}
        onClose={() => setImportOpen(false)}
        io={packIo}
      />
      <ExportModuleDialog
        open={exportOpen}
        onClose={() => setExportOpen(false)}
        models={models}
        io={packIo}
      />

      {/* ── 模型级上传 / 本地路径导入（§6.3）── */}
      <ModelUploadDialog
        open={uploadTarget !== null}
        moduleId={uploadTarget?.moduleId ?? ''}
        moduleName={uploadTarget?.moduleName ?? ''}
        modelId={uploadTarget?.model.model_id ?? ''}
        modelName={uploadTarget?.model.name ?? ''}
        onClose={() => setUploadTarget(null)}
        onSettled={() => void refreshAll()}
      />

      {/* ── 删除模型确认（§5.1 卡内删除）── */}
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open && !deleting) setDeleteTarget(null)
        }}
        variant="destructive"
        title={t('modelDelete.confirmTitle', {
          defaultValue: '删除模型「{{name}}」？',
          name: deleteTarget?.model.name ?? '',
        })}
        description={t('modelDelete.confirmDescription', {
          defaultValue:
            '将删除模块「{{module}}」的模型文件与本机缓存（{{id}}），不可恢复',
          module: deleteTarget?.moduleName ?? '',
          id: deleteTarget?.model.model_id ?? '',
        })}
        confirmLabel={t('common:action.delete')}
        onConfirm={() => confirmDeleteModel()}
      />

      {/* ── 停止模块确认 ── */}
      <ConfirmDialog
        open={stopTarget !== null}
        onOpenChange={(open) => {
          if (!open) setStopTarget(null)
        }}
        variant="destructive"
        title={t('models:module.stopConfirmTitle', {
          defaultValue: '停止模块「{{name}}」？',
          name: stopTarget?.name ?? '',
        })}
        description={t('models:module.stopConfirmDescription', {
          defaultValue: '停止后正在执行的任务可能失败，需重新启动模块才能再次运行',
        })}
        confirmLabel={t('common:action.stop')}
        onConfirm={() => stopModuleConfirmed()}
      />

      {/* ── 卸载来源整合包确认（keep_models 选项）── */}
      <Dialog
        open={uninstallTarget !== null}
        onOpenChange={(open) => {
          if (!open && !uninstalling) setUninstallTarget(null)
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Trash2 className="size-4 text-destructive" />
              {t('packs:uninstall.title', {
                defaultValue: '卸载整合包「{{id}}」？',
                id: uninstallTarget ?? '',
              })}
            </DialogTitle>
            <DialogDescription>
              {t('packs:uninstall.description', {
                defaultValue: '移除注册条目与其安装的管线；模型文件可选保留',
              })}
            </DialogDescription>
          </DialogHeader>
          <label className="flex cursor-pointer items-center gap-2.5 rounded-md border border-border px-3 py-2.5 text-sm">
            <Switch checked={keepModels} onCheckedChange={setKeepModels} />
            {t('packs:uninstall.keepModels', {
              defaultValue: '保留包内安装的模型文件',
            })}
          </label>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setUninstallTarget(null)}
              disabled={uninstalling}
            >
              {t('common:action.cancel')}
            </Button>
            <Button
              variant="destructive"
              disabled={uninstalling}
              onClick={() => void confirmUninstall()}
            >
              {uninstalling ? <Loader2 className="size-4 animate-spin" /> : null}
              {t('packs:action.uninstall', { defaultValue: '卸载' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  )
}
