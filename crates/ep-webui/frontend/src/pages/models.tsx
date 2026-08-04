import { useEffect, useMemo, useRef, useState, type DragEvent } from 'react'
import {
  ChevronDown,
  ChevronRight,
  CircleAlert,
  Database,
  Download,
  FileArchive,
  FileBox,
  FolderOpen,
  FolderUp,
  Gauge,
  HardDrive,
  ListRestart,
  Loader2,
  MemoryStick,
  Package,
  Pencil,
  Play,
  RefreshCw,
  ScrollText,
  SlidersHorizontal,
  Sparkles,
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
import { api } from '@/api/client'
import type {
  CapabilityDecl,
  CapabilityParamSchema,
  ModelInfo,
  ModelSource,
  ModuleResponse,
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
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs'
import { isTaskTerminal, useDirectExec } from '@/hooks/use-direct-exec'
import { useModels } from '@/hooks/use-models'
import {
  useModelDownloads,
  type ModelDownloadProgress,
} from '@/hooks/use-model-downloads'
import { useModules } from '@/hooks/use-modules'
import { categoryLabel, statusMeta } from '@/lib/constants'
import { cn, formatBytes, formatMB } from '@/lib/utils'

// ─── 常量与工具 ──────────────────────────────────────────────────────────────

/** 翻译函数签名（与 react-i18next useTranslation 返回的 t 兼容） */
type TranslateFn = (key: string, options?: Record<string, unknown>) => string

/**
 * 下载状态机（B6 扩展 queued；S2 types.ts 的 ModelDownloadState 尚未同步，
 * 本地放宽联合，落盘见报告仲裁请求）。
 */
type DownloadStateWide = 'queued' | 'downloading' | 'completed' | 'failed' | 'cancelled'

function dlState(download: ModelDownloadProgress | undefined): DownloadStateWide | undefined {
  return download?.state as DownloadStateWide | undefined
}

/** 模型来源 → 展示标签（品牌名 HuggingFace / ModelScope 保持原文，其余走翻译） */
function sourceLabel(t: TranslateFn, source: string): string {
  switch (source.toLowerCase()) {
    case 'huggingface':
      return 'HuggingFace'
    case 'modelscope':
      return 'ModelScope'
    case 'url':
      return t('source.url')
    case 'local_import':
      return t('source.localImport')
    case 'pack':
      return t('source.pack', { defaultValue: '整合包' })
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

function normalizeStatus(status: string): string {
  return status.trim().toLowerCase()
}

function isReadyStatus(status: string): boolean {
  return normalizeStatus(status) === 'ready'
}

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

/** 压缩包文件名（服务端解包：.zip / .tar.gz / .tgz） */
const ARCHIVE_PATTERN = /\.(zip|tar\.gz|tgz)$/i

function totalSize(files: File[]): number {
  return files.reduce((sum, f) => sum + f.size, 0)
}

/**
 * 若所有路径共享同一个顶层目录则剥掉一层（与服务端压缩包解包的
 * "单一顶层目录剥层" 行为对齐，保证所选文件夹本身即模型根）。
 */
function stripCommonTopDir(paths: string[]): string[] {
  if (paths.length === 0) return paths
  const slash = paths[0].indexOf('/')
  if (slash <= 0) return paths
  const top = paths[0].slice(0, slash)
  if (!paths.every((p) => p.startsWith(`${top}/`))) return paths
  return paths.map((p) => p.slice(top.length + 1))
}

/** tag 客户端归一化（B6 服务端同款语义：trim、去空、保序去重） */
function normalizeTags(tags: string[]): string[] {
  const out: string[] = []
  for (const raw of tags) {
    const tag = raw.trim()
    if (!tag || out.includes(tag)) continue
    out.push(tag)
  }
  return out
}

// ─── 拖拽文件递归收集 ────────────────────────────────────────────────────────

interface CollectedFile {
  file: File
  /** 相对路径（含所拖入文件夹的层级） */
  path: string
}

/**
 * 递归遍历 DataTransfer 条目（文件 / 目录），收集全部文件与相对路径。
 *
 * 注意：`webkitGetAsEntry()` 必须在 drop 事件同步阶段调用，
 * 因此先同步取出全部 entry，再异步遍历。
 */
async function collectDataTransferItems(
  items: DataTransferItemList,
): Promise<CollectedFile[]> {
  const entries: FileSystemEntry[] = []
  for (let i = 0; i < items.length; i++) {
    const entry = items[i].webkitGetAsEntry()
    if (entry) entries.push(entry)
  }
  const out: CollectedFile[] = []
  if (entries.length > 0) {
    for (const entry of entries) {
      await walkFileSystemEntry(entry, '', out)
    }
    return out
  }
  // 兜底：浏览器不支持 webkitGetAsEntry 时按普通文件读取
  for (let i = 0; i < items.length; i++) {
    const file = items[i].getAsFile()
    if (file) out.push({ file, path: file.name })
  }
  return out
}

/** 递归遍历单个 FileSystemEntry，把文件追加进 out */
function walkFileSystemEntry(
  entry: FileSystemEntry,
  prefix: string,
  out: CollectedFile[],
): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    if (entry.isFile) {
      ;(entry as FileSystemFileEntry).file(
        (file) => {
          out.push({ file, path: prefix + entry.name })
          resolve()
        },
        (e) => reject(e),
      )
      return
    }
    if (!entry.isDirectory) {
      resolve()
      return
    }
    const reader = (entry as FileSystemDirectoryEntry).createReader()
    // readEntries 单次只返回部分条目，必须反复调用直至返回空数组
    const readBatch = () => {
      reader.readEntries(
        (batch) => {
          if (batch.length === 0) {
            resolve()
            return
          }
          ;(async () => {
            for (const child of batch) {
              await walkFileSystemEntry(child, `${prefix}${entry.name}/`, out)
            }
            readBatch()
          })().catch(reject)
        },
        (e) => reject(e),
      )
    }
    readBatch()
  })
}

// ─── XHR 上传（真实进度）────────────────────────────────────────────────────

export interface UploadProgress {
  percent: number
  loaded: number
  total: number
}

/** 上传取消的内部错误标识（与语言无关，仅用于分支判断，不直接展示） */
const UPLOAD_ABORTED = '__ep_upload_aborted__'

/**
 * 以 XMLHttpRequest 上传模型文件，换取真实的上传进度事件
 * （fetch 无法获取上传进度；模型动辄数 GB，进度反馈是必需的）。
 *
 * 表单结构与 `api.uploadModel` 保持一致：model_id + files[] + paths[]。
 */
function uploadModelWithProgress(
  moduleId: string,
  modelId: string,
  files: File[],
  paths: string[],
  onProgress: (p: UploadProgress) => void,
  t: TranslateFn,
): { promise: Promise<ModelInfo>; abort: () => void } {
  const form = new FormData()
  form.append('model_id', modelId)
  files.forEach((file, i) => {
    form.append('files', file)
    form.append('paths', paths[i] || file.webkitRelativePath || file.name)
  })
  const xhr = new XMLHttpRequest()
  const promise = new Promise<ModelInfo>((resolve, reject) => {
    xhr.open('POST', `/api/models/${encodeURIComponent(moduleId)}/upload`)
    xhr.upload.addEventListener('progress', (e) => {
      if (!e.lengthComputable) return
      onProgress({
        loaded: e.loaded,
        total: e.total,
        percent: e.total > 0 ? Math.min(100, (e.loaded / e.total) * 100) : 0,
      })
    })
    xhr.addEventListener('load', () => {
      let body: unknown = null
      try {
        body = JSON.parse(xhr.responseText)
      } catch {
        // 非 JSON 响应
      }
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve((body ?? {}) as ModelInfo)
        return
      }
      const msg =
        body && typeof body === 'object' && 'error' in body
          ? (body as { error: unknown }).error
          : null
      reject(
        new Error(
          typeof msg === 'string' && msg.trim()
            ? msg
            : t('upload.errorHttp', { status: xhr.status }),
        ),
      )
    })
    xhr.addEventListener('error', () => reject(new Error(t('upload.errorNetwork'))))
    xhr.addEventListener('abort', () => reject(new Error(UPLOAD_ABORTED)))
    xhr.send(form)
  })
  return { promise, abort: () => xhr.abort() }
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
  const { t } = useTranslation('models')
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
  const { t } = useTranslation('models')
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

function SourceBadges({ model }: { model: ModelInfo }) {
  const { t } = useTranslation('models')
  // 多源时展示全部可用源；否则回退 source 字段（未知来源原样显示）
  const sources: string[] =
    model.available_sources && model.available_sources.length > 0
      ? model.available_sources
      : model.source
        ? [model.source]
        : []
  if (sources.length === 0) return null
  return (
    <>
      {sources.map((s) => (
        <Badge
          key={s}
          variant="outline"
          className="border-border/60 px-1.5 text-[10px] font-normal text-muted-foreground"
        >
          {sourceLabel(t, s)}
        </Badge>
      ))}
    </>
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

// ─── 直跑抽屉（§5.3）────────────────────────────────────────────────────────

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
      <Select
        value={current}
        onValueChange={(v) => onChange(v)}
      >
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
          const num = type === 'integer' ? Number.parseInt(raw, 10) : Number.parseFloat(raw)
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

/** 直跑抽屉节点 id → 展示名（退化三节点 DAG，B3 build_direct_pipeline 契约） */
const DIRECT_NODE_LABEL_KEYS: Record<string, string> = {
  input: 'run.nodeInput',
  run: 'run.nodeRun',
  output: 'run.nodeOutput',
}

interface DirectRunDrawerProps {
  module: ModuleResponse | null
  onClose: () => void
}

/**
 * 单模型直跑抽屉（§5.3）：
 * 选 capability（裸名，来自 ModuleResponse.capabilities）→ 按 params schema
 * 渲染参数表单（预填 default）→ 输入文件（本地路径 / 浏览器上传回填 path）
 * → executeSingle（后端未运行时自动拉起模块并同步等健康，给足 fetch 超时）
 * → WS progress 按 task_id 过滤进度 → 产物预览/下载。
 */
function DirectRunDrawer({ module, onClose }: DirectRunDrawerProps) {
  const { t } = useTranslation('models')
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
    // eslint 不识别本文件内自定义 hook 依赖描述，exec.reset 为稳定引用
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
      toast.success(t('run.uploadDone', { defaultValue: '输入文件已上传' }), {
        description: resp.path,
      })
    } catch (e) {
      toast.error(t('run.uploadFailed', { defaultValue: '输入文件上传失败' }), {
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
      toast.success(t('run.accepted', { defaultValue: '直跑任务已提交' }), {
        description: task,
      })
    } else if (exec.submitError === '__ep_direct_exec_submit_timeout__') {
      toast.error(t('run.submitTimeout', { defaultValue: '等待模块启动超时' }), {
        description: t('run.submitTimeoutDesc', {
          defaultValue: '模块自动拉起耗时超出预期，可稍后在任务页查看或重试',
        }),
      })
    } else if (exec.submitError) {
      toast.error(t('run.submitFailed', { defaultValue: '直跑提交失败' }), {
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
    () => Object.entries(currentCap?.params ?? {}).sort(([a], [b]) => a.localeCompare(b)),
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
            {t('run.title', { defaultValue: '单模型直跑' })}
            {module ? <span className="text-muted-foreground">· {module.name}</span> : null}
          </SheetTitle>
          <SheetDescription>
            {t('run.description', {
              defaultValue: '选择能力与参数后直接执行，模块未运行时将自动拉起',
            })}
          </SheetDescription>
        </SheetHeader>

        {capabilities.length === 0 ? (
          <p className="px-4 text-sm text-muted-foreground">
            {t('run.noCapabilities', { defaultValue: '该模块未声明任何能力，无法直跑' })}
          </p>
        ) : (
          <div className="space-y-5 px-4 pb-6">
            {/* 1. 能力选择（裸名，来自 manifest capabilities） */}
            <div className="space-y-2">
              <label className="text-sm font-medium">
                {t('run.capability', { defaultValue: '能力' })}
              </label>
              <Select
                value={capability || undefined}
                onValueChange={handleCapabilityChange}
                disabled={exec.submitting}
              >
                <SelectTrigger className="w-full">
                  <SelectValue
                    placeholder={t('run.selectCapability', { defaultValue: '选择能力' })}
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
                  {t('run.params', { defaultValue: '参数' })}
                </label>
                {paramEntries.map(([name, schema]) => (
                  <div key={name} className="space-y-1">
                    <div className="flex items-center justify-between gap-2">
                      <span className="font-mono text-xs text-foreground/80">{name}</span>
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
                      onChange={(v) =>
                        setParams((prev) => ({ ...prev, [name]: v }))
                      }
                    />
                    {schema.description ? (
                      <p className="text-[11px] text-muted-foreground">{schema.description}</p>
                    ) : null}
                  </div>
                ))}
              </div>
            )}

            {/* 3. 输入文件：本地路径直填 或 浏览器上传回填 path */}
            <div className="space-y-2">
              <label className="text-sm font-medium">
                {t('run.input', { defaultValue: '输入文件' })}
              </label>
              <div className="flex gap-2">
                <Input
                  value={inputPath}
                  onChange={(e) => setInputPath(e.target.value)}
                  placeholder={t('run.inputPlaceholder', {
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
                  {t('run.upload', { defaultValue: '上传' })}
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
                {t('run.inputHint', {
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
                  ? t('run.startingModule', {
                      defaultValue: '正在启动模块并提交任务…',
                    })
                  : t('run.submit', { defaultValue: '执行' })}
              </Button>
              {exec.submitting && (
                <p className="text-[11px] text-muted-foreground">
                  {t('run.startingModuleHint', {
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
                      ? t(`common:status.${taskStatus === 'queued' ? 'pending' : taskStatus}`, {
                          defaultValue: taskStatus,
                        })
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
                                ? '输入文件'
                                : nodeId === 'run'
                                  ? '模块执行'
                                  : '结果输出',
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
                    {t('run.progressHint', {
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
                  {t('run.artifacts', { defaultValue: '结果产物' })}
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
                        {t('run.preview', { defaultValue: '预览' })}
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
                {preview?.name ?? t('run.preview', { defaultValue: '预览' })}
              </DialogTitle>
              <DialogDescription>
                {preview ? formatBytes(preview.size) : ''}
              </DialogDescription>
            </DialogHeader>
            {previewError ? (
              <p className="text-sm text-status-error">
                {t('run.previewFailed', { defaultValue: '预览失败' })}：{previewError}
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
                {t('run.previewBinary', {
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
  const { t } = useTranslation('models')
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
            {t('detail.drawerSuffix', { defaultValue: ' · 模块详情' })}
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
              {t('detail.capabilities', { defaultValue: '能力声明' })}
            </h3>
            {capabilities.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {t('detail.noCapabilities', {
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
                              {t('detail.paramName', { defaultValue: '参数' })}
                            </th>
                            <th className="py-1 pr-3 font-normal">
                              {t('detail.paramType', { defaultValue: '类型' })}
                            </th>
                            <th className="py-1 pr-3 font-normal">
                              {t('detail.paramDefault', { defaultValue: '默认值' })}
                            </th>
                            <th className="py-1 font-normal">
                              {t('detail.paramConstraint', { defaultValue: '约束' })}
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
                                    {schema.default === undefined || schema.default === null
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
  const { t } = useTranslation('models')
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
            {t('logs.title', { defaultValue: '模块日志' })} · {moduleName}
          </SheetTitle>
          <SheetDescription>
            {t('logs.description', {
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
  const { t } = useTranslation('models')
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
      toast.success(t('tags.saved', { defaultValue: '标签已保存' }), {
        description: target.model.name,
      })
      onSaved()
      onClose()
    } catch (e) {
      toast.error(t('tags.saveFailed', { defaultValue: '保存标签失败' }), {
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
            {t('tags.editTitle', { defaultValue: '编辑标签' })}
          </DialogTitle>
          <DialogDescription>
            {t('tags.editDescription', {
              defaultValue:
                '标签存入模型元数据，随整合包流转；保存为全量覆写（空列表 = 清空）',
            })}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div className="flex min-h-9 flex-wrap items-center gap-1.5 rounded-md border border-border bg-muted/20 p-2">
            {tags.length === 0 ? (
              <span className="text-xs text-muted-foreground">
                {t('tags.empty', { defaultValue: '暂无标签' })}
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
              placeholder={t('tags.placeholder', {
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
              {t('tags.add', { defaultValue: '添加' })}
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

// ─── 页面状态类型 ────────────────────────────────────────────────────────────

/** 已选择待上传的文件集合 */
interface PickedUpload {
  /** folder=文件夹逐文件上传；archive=单个压缩包（服务端解包） */
  mode: 'folder' | 'archive'
  files: File[]
  /** 与 files 同序的相对路径 */
  paths: string[]
  totalBytes: number
}

interface UpdateCheckResult {
  available: boolean
  reason: string
}

/** GET /api/config 的 active_models 扩展（S2 AppConfig 类型未含该字段，契约见 §8.3） */
type ActiveModelsMap = Record<string, string>

// ─── 页面 ────────────────────────────────────────────────────────────────────

export function ModelsPage() {
  const { t } = useTranslation('models')
  // 数据层：保留 use-models hook（仅重构呈现，降回归风险）
  const {
    models,
    details,
    moduleModels,
    importModel,
    deleteModel,
    refresh,
    loading,
    error,
  } = useModels()
  // 模块维度（统一页卡片 = (模块, 激活模型) 投影，模块运行状态来自 5s 轮询）
  const { modules, statusMap, loading: modulesLoading, refresh: refreshModules } =
    useModules()

  /** 每模块激活变体（config.active_models，§5.2 单槽位） */
  const [activeModels, setActiveModels] = useState<ActiveModelsMap>({})
  /** tag chips 筛选（多选 OR） */
  const [tagFilters, setTagFilters] = useState<Set<string>>(new Set())

  /** 已展开的模型详情行（key = module_id/model_id） */
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [detailLoading, setDetailLoading] = useState<Record<string, boolean>>({})

  // ── 抽屉 / 对话框 ──
  const [logsModuleId, setLogsModuleId] = useState<string | null>(null)
  const [detailModule, setDetailModule] = useState<ModuleResponse | null>(null)
  const [runModule, setRunModule] = useState<ModuleResponse | null>(null)
  const [tagsTarget, setTagsTarget] = useState<TagsTarget | null>(null)
  const [stopTarget, setStopTarget] = useState<ModuleResponse | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<{
    moduleId: string
    model: ModelInfo
  } | null>(null)

  // ── 服务器本地路径导入表单 ──
  const [importModule, setImportModule] = useState<string>('')
  const [importModelId, setImportModelId] = useState<string>('')
  const [sourcePath, setSourcePath] = useState('')
  const [importing, setImporting] = useState(false)

  // ── 从本机上传 ──
  const [uploadModule, setUploadModule] = useState<string>('')
  const [uploadModelId, setUploadModelId] = useState<string>('')
  const [picked, setPicked] = useState<PickedUpload | null>(null)
  const [collecting, setCollecting] = useState(false)
  const [dragOver, setDragOver] = useState(false)
  const [uploading, setUploading] = useState<UploadProgress | null>(null)
  const folderInputRef = useRef<HTMLInputElement | null>(null)
  const archiveInputRef = useRef<HTMLInputElement | null>(null)
  const abortUploadRef = useRef<(() => void) | null>(null)

  // ── 更新检查（页面状态，不持久化）──
  const [updateInfo, setUpdateInfo] = useState<Record<string, UpdateCheckResult>>({})
  const [checking, setChecking] = useState<Set<string>>(new Set())
  const [checkingAll, setCheckingAll] = useState(false)

  // 挂载时读取 config.active_models（变体单槽位真源之一）
  useEffect(() => {
    api
      .getConfig()
      .then((cfg) => {
        const extra = cfg as typeof cfg & { active_models?: ActiveModelsMap }
        setActiveModels(extra.active_models ?? {})
      })
      .catch(() => {
        // 配置读取失败：变体选择器退化为清单首项推断
      })
  }, [])

  /** 某模块缺失模型数量 > 0 时展示获取途径引导 */
  const modelStats = useMemo(() => {
    let total = 0
    let missing = 0
    let ready = 0
    for (const group of models?.modules ?? []) {
      for (const model of group.models) {
        total++
        const s = normalizeStatus(model.status)
        if (s === 'missing' || s === 'incomplete') missing++
        if (s === 'ready') ready++
      }
    }
    return { total, missing, ready }
  }, [models])

  /** 全部 tag（chips 筛选用，按出现顺序去重） */
  const allTags = useMemo(() => {
    const out: string[] = []
    for (const group of models?.modules ?? []) {
      for (const model of group.models) {
        for (const tag of model.tags ?? []) {
          if (!out.includes(tag)) out.push(tag)
        }
      }
    }
    return out.sort((a, b) => a.localeCompare(b))
  }, [models])

  /** module_id → 模型分组（仅含声明了模型的模块） */
  const groupByModule = useMemo(() => {
    const map = new Map<string, { module_id: string; module_name: string; models: ModelInfo[] }>()
    for (const group of models?.modules ?? []) {
      map.set(group.module_id, group)
    }
    return map
  }, [models])

  // ── 下载状态：WS 推送为主 + 挂载/轮询兜底 ──
  function handleDownloadSettled(entry: ModelDownloadProgress) {
    // 终态到达：刷新模型列表（详情已展开则一并同步）+ 结果 toast
    void refresh()
    if (details[entry.module_id]) {
      void moduleModels(entry.module_id).catch(() => {})
    }
    const group = models?.modules.find((g) => g.module_id === entry.module_id)
    const name =
      group?.models.find((m) => m.model_id === entry.model_id)?.name ??
      entry.model_id
    if (entry.state === 'completed') {
      toast.success(t('download.completed'), { description: name })
    } else if (entry.state === 'failed') {
      toast.error(t('download.failed'), {
        description: t('download.failedDesc', { name }),
      })
    } else {
      toast.info(t('download.cancelled'), { description: name })
    }
  }

  const { get: getDownload, refresh: refreshDownloads } =
    useModelDownloads(handleDownloadSettled)

  // ── 模块操作 ──
  async function startModule(m: ModuleResponse) {
    try {
      const res = await api.startModule(m.id)
      if (res.error) throw new Error(res.error)
      toast.success(t('module.startStarted', { defaultValue: '模块启动中' }), {
        description: m.name,
      })
      await refreshModules()
    } catch (e) {
      toast.error(t('module.startFailed', { defaultValue: '模块启动失败' }), {
        description: errMsg(e),
      })
    }
  }

  async function stopModuleConfirmed() {
    if (!stopTarget) return
    try {
      const res = await api.stopModule(stopTarget.id)
      if (res.error) throw new Error(res.error)
      toast.success(t('module.stopSucceeded', { defaultValue: '模块已停止' }), {
        description: stopTarget.name,
      })
      await refreshModules()
    } catch (e) {
      toast.error(t('module.stopFailed', { defaultValue: '模块停止失败' }), {
        description: errMsg(e),
      })
      throw e // 保持确认框打开，便于重试
    }
  }

  // ── 变体单槽位切换（§5.2 / B5 端点）──
  async function switchVariant(moduleId: string, modelId: string) {
    const module = modules.find((m) => m.id === moduleId)
    try {
      const resp = await api.setModelVariant(moduleId, modelId, { model_id: modelId })
      setActiveModels((prev) => ({ ...prev, [moduleId]: modelId }))
      await Promise.allSettled([refresh(), refreshModules()])
      if (resp.needs_download) {
        // 引导下载：直接按主源启动目标变体下载
        toast.warning(
          t('variant.needsDownload', { defaultValue: '该变体本地缺失，需要下载' }),
          {
            description: modelId,
            action: {
              label: t('variant.downloadNow', { defaultValue: '立即下载' }),
              onClick: () => {
                const group = groupByModule.get(moduleId)
                const model = group?.models.find((m) => m.model_id === modelId)
                if (model) void startDownload(moduleId, model)
              },
            },
          },
        )
      } else if (resp.needs_restart) {
        toast.info(
          t('variant.needsRestart', { defaultValue: '变体已切换，重启模块后生效' }),
          {
            description: module?.name ?? moduleId,
            action: module
              ? {
                  label: t('variant.restartNow', { defaultValue: '立即重启' }),
                  onClick: () => void restartModule(module),
                }
              : undefined,
          },
        )
      } else {
        toast.success(t('variant.switchSuccess', { defaultValue: '激活变体已切换' }), {
          description: modelId,
        })
      }
    } catch (e) {
      toast.error(t('variant.switchFailed', { defaultValue: '变体切换失败' }), {
        description: errMsg(e),
      })
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

  // ── 下载 ──
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
      toast.success(t('download.started'), {
        description: chosen
          ? `${model.name} · ${sourceLabel(t, chosen)}`
          : model.name,
      })
      // 立即拉一次下载列表，保证进度条尽快出现（WS 初始推送可能稍晚）
      void refreshDownloads()
    } catch (e) {
      toast.error(t('download.startFailed'), { description: errMsg(e) })
    }
  }

  // ── 取消下载（queued 与 downloading 均可取消，B6）──
  async function cancelDownload(moduleId: string, model: ModelInfo) {
    try {
      await api.cancelModelDownload(moduleId, model.model_id)
      toast.info(t('download.cancelRequested', { defaultValue: '已请求取消下载' }), {
        description: model.name,
      })
      void refreshDownloads()
    } catch (e) {
      toast.error(t('download.cancelFailed', { defaultValue: '取消下载失败' }), {
        description: errMsg(e),
      })
    }
  }

  // ── 删除 ──
  async function confirmDelete() {
    if (!deleteTarget) return
    try {
      await deleteModel(deleteTarget.moduleId, deleteTarget.model.model_id)
      toast.success(t('delete.success'), { description: deleteTarget.model.name })
      const key = `${deleteTarget.moduleId}/${deleteTarget.model.model_id}`
      setUpdateInfo((prev) => {
        if (!(key in prev)) return prev
        const next = { ...prev }
        delete next[key]
        return next
      })
    } catch (e) {
      toast.error(t('delete.failed'), { description: errMsg(e) })
      throw e // 保持确认框打开，便于重试
    }
  }

  // ── 更新检查 ──
  async function checkOne(moduleId: string, model: ModelInfo) {
    const key = `${moduleId}/${model.model_id}`
    setChecking((prev) => new Set(prev).add(key))
    try {
      const result = await api.checkModelUpdate(moduleId, model.model_id)
      setUpdateInfo((prev) => ({ ...prev, [key]: result }))
      if (result.available) {
        toast.info(t('update.availableToast', { name: model.name }), {
          description: result.reason,
        })
      } else {
        toast.success(t('update.upToDateToast', { name: model.name }), {
          description: result.reason,
        })
      }
    } catch (e) {
      toast.error(t('update.checkFailed'), { description: errMsg(e) })
    } finally {
      setChecking((prev) => {
        const next = new Set(prev)
        next.delete(key)
        return next
      })
    }
  }

  async function checkAll() {
    const targets: { moduleId: string; model: ModelInfo }[] = []
    for (const group of models?.modules ?? []) {
      for (const model of group.models) {
        if (isReadyStatus(model.status)) {
          targets.push({ moduleId: group.module_id, model })
        }
      }
    }
    if (targets.length === 0) {
      toast.info(t('update.noneToCheck'), {
        description: t('update.onlyReadyCheckable'),
      })
      return
    }
    setCheckingAll(true)
    try {
      const results = await Promise.allSettled(
        targets.map((target) =>
          api.checkModelUpdate(target.moduleId, target.model.model_id),
        ),
      )
      const patch: Record<string, UpdateCheckResult> = {}
      const updatable: string[] = []
      let failed = 0
      results.forEach((r, i) => {
        const target = targets[i]
        const key = `${target.moduleId}/${target.model.model_id}`
        if (r.status === 'fulfilled') {
          patch[key] = r.value
          if (r.value.available) updatable.push(target.model.name)
        } else {
          failed++
        }
      })
      setUpdateInfo((prev) => ({ ...prev, ...patch }))
      if (updatable.length > 0) {
        toast.warning(t('update.updatableCount', { count: updatable.length }), {
          description: updatable.join(t('update.listSeparator')),
        })
      } else if (failed > 0) {
        toast.error(t('update.someCheckFailed', { count: failed }))
      } else {
        toast.success(t('update.allUpToDate'))
      }
    } finally {
      setCheckingAll(false)
    }
  }

  // ── 服务器本地路径导入 ──
  async function handleImport() {
    if (!importModule || !importModelId || !sourcePath.trim()) return
    setImporting(true)
    const toastId = toast.loading(t('import.importingToast'))
    try {
      const resp = await importModel(importModule, {
        model_id: importModelId,
        source_path: sourcePath.trim(),
      })
      if (resp.error) {
        toast.error(t('import.failed'), { id: toastId, description: resp.error })
      } else {
        toast.success(t('import.success'), {
          id: toastId,
          description: t('add.filesAndSize', {
            count: resp.file_count ?? '–',
            size: formatBytes(resp.total_bytes),
          }),
        })
        setImportModelId('')
        setSourcePath('')
      }
    } catch (e) {
      toast.error(t('import.failed'), {
        id: toastId,
        description: e instanceof Error ? e.message : String(e),
      })
    } finally {
      setImporting(false)
    }
  }

  // ── 从本机上传：文件收集 ──
  function handleFilesPicked(files: File[]) {
    if (files.length === 0) return
    // 单个压缩包 → 归档模式（服务端解包）
    if (files.length === 1 && ARCHIVE_PATTERN.test(files[0].name)) {
      setPicked({
        mode: 'archive',
        files,
        paths: [files[0].name],
        totalBytes: files[0].size,
      })
      return
    }
    const paths = stripCommonTopDir(
      files.map((f) => f.webkitRelativePath || f.name),
    )
    setPicked({ mode: 'folder', files, paths, totalBytes: totalSize(files) })
  }

  async function handleDrop(e: DragEvent<HTMLDivElement>) {
    e.preventDefault()
    setDragOver(false)
    if (uploading || collecting) return
    const items = e.dataTransfer?.items
    if (!items || items.length === 0) return
    setCollecting(true)
    try {
      const collected = await collectDataTransferItems(items)
      if (collected.length === 0) {
        toast.error(t('upload.dropEmpty'))
        return
      }
      const isSingleArchive =
        collected.length === 1 &&
        ARCHIVE_PATTERN.test(collected[0].file.name) &&
        !collected[0].path.includes('/')
      if (isSingleArchive) {
        setPicked({
          mode: 'archive',
          files: [collected[0].file],
          paths: [collected[0].file.name],
          totalBytes: collected[0].file.size,
        })
      } else {
        const files = collected.map((c) => c.file)
        setPicked({
          mode: 'folder',
          files,
          paths: stripCommonTopDir(collected.map((c) => c.path)),
          totalBytes: totalSize(files),
        })
      }
    } catch (err) {
      toast.error(t('upload.dropReadFailed'), { description: errMsg(err) })
    } finally {
      setCollecting(false)
    }
  }

  // ── 从本机上传：提交 ──
  async function handleUpload() {
    if (!uploadModule || !uploadModelId || !picked || uploading || collecting) {
      return
    }
    const { files, paths, totalBytes } = picked
    const moduleId = uploadModule
    setUploading({ percent: 0, loaded: 0, total: totalBytes })
    const toastId = toast.loading(t('upload.uploadingToast'))
    const { promise, abort } = uploadModelWithProgress(
      moduleId,
      uploadModelId,
      files,
      paths,
      setUploading,
      t,
    )
    abortUploadRef.current = abort
    try {
      await promise
      toast.success(t('upload.success'), {
        id: toastId,
        description: t('add.filesAndSize', {
          count: files.length,
          size: formatBytes(totalBytes),
        }),
      })
      setPicked(null)
      setUploadModelId('')
      // 刷新列表；该模块详情已展开则一并同步
      await Promise.allSettled([
        refresh(),
        details[moduleId] ? moduleModels(moduleId) : Promise.resolve(),
      ])
    } catch (e) {
      if (e instanceof Error && e.message === UPLOAD_ABORTED) {
        toast.info(t('upload.cancelled'), { id: toastId })
      } else {
        toast.error(t('upload.failed'), { id: toastId, description: errMsg(e) })
      }
    } finally {
      abortUploadRef.current = null
      setUploading(null)
    }
  }

  function toggleExpand(moduleId: string, model: ModelInfo) {
    const key = `${moduleId}/${model.model_id}`
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(key)) {
        next.delete(key)
      } else {
        next.add(key)
        // 首次展开时拉取该模块的模型详情
        if (!details[moduleId] && !detailLoading[moduleId]) {
          setDetailLoading((s) => ({ ...s, [moduleId]: true }))
          void moduleModels(moduleId)
            .catch((e: unknown) => {
              toast.error(
                t('detail.loadFailed', {
                  error: e instanceof Error ? e.message : String(e),
                }),
              )
            })
            .finally(() => {
              setDetailLoading((s) => ({ ...s, [moduleId]: false }))
            })
        }
      }
      return next
    })
  }

  /** tag chips 筛选开关（多选 OR） */
  function toggleTagFilter(tag: string) {
    setTagFilters((prev) => {
      const next = new Set(prev)
      if (next.has(tag)) next.delete(tag)
      else next.add(tag)
      return next
    })
  }

  /** 卡片是否通过 tag 筛选（无筛选时全部通过；native 模块无 tag，筛选激活时隐藏） */
  function passesTagFilter(moduleId: string): boolean {
    if (tagFilters.size === 0) return true
    const group = groupByModule.get(moduleId)
    if (!group) return false
    return group.models.some((m) =>
      (m.tags ?? []).some((tag) => tagFilters.has(tag)),
    )
  }

  /**
   * 解析模块激活变体（§5.2 单槽位）：
   * 显式 pin（config.active_models）优先；未配置时回退清单首个变体
   * （现网 manifest 均 default=true 在首项；后端解析含 default 回退，
   * 精确值待后端暴露解析结果——见报告仲裁请求）。
   */
  function resolveActive(moduleId: string): {
    model: ModelInfo | null
    assumed: boolean
  } {
    const group = groupByModule.get(moduleId)
    if (!group || group.models.length === 0) return { model: null, assumed: false }
    const pinned = activeModels[moduleId]
    if (pinned) {
      const found = group.models.find((m) => m.model_id === pinned)
      if (found) return { model: found, assumed: false }
    }
    return { model: group.models[0], assumed: true }
  }

  const moduleOptions = models?.modules ?? []
  const uploadModelOptions =
    moduleOptions.find((m) => m.module_id === uploadModule)?.models ?? []
  const importModelOptions =
    moduleOptions.find((m) => m.module_id === importModule)?.models ?? []

  const visibleModules = modules.filter((m) => passesTagFilter(m.id))
  const pageLoading = loading || modulesLoading

  // ── 变体行（非激活变体；与激活面板共享下载/取消/删除/更新/tag 操作）──
  function renderVariantRow(moduleId: string, model: ModelInfo) {
    const key = `${moduleId}/${model.model_id}`
    const isOpen = expanded.has(key)
    const detail = details[moduleId]?.models.find(
      (d) => d.model_id === model.model_id,
    )
    const download = getDownload(moduleId, model.model_id)
    const state = dlState(download)
    const isActiveDl = state === 'downloading' || state === 'queued'
    const status = normalizeStatus(model.status)
    const canDownload = (status === 'missing' || status === 'incomplete') && !isActiveDl
    const sources = model.available_sources ?? []
    const update = updateInfo[key]

    return (
      <div key={model.model_id}>
        <div className="flex w-full flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2.5">
          <button
            type="button"
            onClick={() => toggleExpand(moduleId, model)}
            className="shrink-0 rounded p-0.5 hover:bg-muted"
            aria-label={t('detail.expand', { defaultValue: '展开详情' })}
          >
            <ChevronRight
              className={cn(
                'size-3.5 text-muted-foreground transition-transform',
                isOpen && 'rotate-90',
              )}
            />
          </button>
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 items-center gap-2">
              <span className="truncate text-sm font-medium">{model.name}</span>
              {update?.available && (
                <Badge
                  variant="outline"
                  className="border-status-preparing/30 bg-status-preparing/15 px-1.5 text-[10px] text-status-preparing"
                >
                  <Sparkles className="size-2.5" />
                  {t('update.badge')}
                </Badge>
              )}
            </div>
            <div className="mt-0.5 flex min-w-0 flex-wrap items-center gap-1.5">
              <span className="truncate font-mono text-xs text-muted-foreground">
                {model.model_id}
              </span>
              <SourceBadges model={model} />
            </div>
          </div>

          <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">
            <ModelStatusBadge status={state === 'downloading' ? 'downloading' : model.status} />
            {isActiveDl && download ? (
              <div className="w-36 sm:w-44">
                <div className="mb-1 flex items-center justify-between font-mono text-[11px] text-muted-foreground">
                  {state === 'queued' ? (
                    <span className="animate-pulse">
                      {t('common:status.queued', { defaultValue: '排队中' })}
                    </span>
                  ) : (
                    <span>{Math.floor(download.percent)}%</span>
                  )}
                  <span>{formatBytes(download.bytes)}</span>
                </div>
                <Progress
                  value={state === 'queued' ? 0 : download.percent}
                  className={cn('h-1.5', state === 'queued' && 'animate-pulse')}
                />
              </div>
            ) : (
              <span className="hidden w-16 text-right font-mono text-xs text-muted-foreground sm:block">
                {formatMB(model.size_estimate_mb)}
              </span>
            )}

            {/* 取消下载（queued 与 downloading 均显示，B6） */}
            {isActiveDl && (
              <Button
                size="xs"
                variant="ghost"
                onClick={() => void cancelDownload(moduleId, model)}
              >
                <X className="size-3" />
                {t('common:action.cancel')}
              </Button>
            )}

            {/* 下载：多源先选源，单源直接下载 */}
            {canDownload &&
              (sources.length > 1 ? (
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button size="xs">
                      <Download className="size-3" />
                      {t('common:action.download')}
                      <ChevronDown className="size-3" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    {sources.map((s) => (
                      <DropdownMenuItem
                        key={s}
                        onSelect={() => void startDownload(moduleId, model, s)}
                      >
                        <Download className="size-3.5" />
                        {sourceLabel(t, s)}
                      </DropdownMenuItem>
                    ))}
                  </DropdownMenuContent>
                </DropdownMenu>
              ) : (
                <Button size="xs" onClick={() => void startDownload(moduleId, model, sources[0])}>
                  <Download className="size-3" />
                  {t('common:action.download')}
                </Button>
              ))}

            {/* 有更新：重新下载（走主源下载流程） */}
            {update?.available && !isActiveDl && (
              <Button
                size="xs"
                variant="outline"
                onClick={() => void startDownload(moduleId, model, primarySource(model))}
              >
                <Download className="size-3" />
                {t('download.redownload')}
              </Button>
            )}

            {/* 就绪模型：检查更新 + tag + 删除 */}
            {status === 'ready' && (
              <>
                <Button
                  size="xs"
                  variant="ghost"
                  disabled={checking.has(key) || checkingAll}
                  onClick={() => void checkOne(moduleId, model)}
                >
                  {checking.has(key) ? (
                    <Loader2 className="size-3 animate-spin" />
                  ) : (
                    <Sparkles className="size-3" />
                  )}
                  {t('update.check')}
                </Button>
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={() => setTagsTarget({ moduleId, model })}
                >
                  <Tag className="size-3" />
                  {t('tags.action', { defaultValue: '标签' })}
                </Button>
                <Button
                  size="xs"
                  variant="ghost"
                  className="text-muted-foreground hover:text-destructive"
                  onClick={() => setDeleteTarget({ moduleId, model })}
                >
                  <Trash2 className="size-3" />
                  {t('common:action.delete')}
                </Button>
              </>
            )}
          </div>
        </div>
        {isOpen && (
          <div className="border-t border-dashed border-border bg-muted/30 px-6 py-3 pl-12">
            {detailLoading[moduleId] && !detail ? (
              <div className="space-y-2">
                <Skeleton className="h-4 w-48" />
                <Skeleton className="h-4 w-64" />
              </div>
            ) : detail ? (
              <dl className="grid gap-x-8 gap-y-2 text-sm sm:grid-cols-3">
                <div>
                  <dt className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <FileBox className="size-3.5" />
                    {t('detail.fileCount')}
                  </dt>
                  <dd className="mt-1 font-mono">{detail.file_count ?? '–'}</dd>
                </div>
                <div>
                  <dt className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <HardDrive className="size-3.5" />
                    {t('detail.actualSize')}
                  </dt>
                  <dd className="mt-1 font-mono">{formatBytes(detail.size_bytes)}</dd>
                </div>
                <div className="min-w-0">
                  <dt className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <FolderOpen className="size-3.5" />
                    {t('detail.cachePath')}
                  </dt>
                  <dd className="mt-1 break-all font-mono text-xs leading-relaxed">
                    {detail.local_cache_path ?? detail.target_dir}
                  </dd>
                </div>
              </dl>
            ) : (
              <p className="text-xs text-muted-foreground">{t('detail.noData')}</p>
            )}
          </div>
        )}
      </div>
    )
  }

  // ── 统一卡片：(模块, 激活模型) 投影（§5.1）──
  function renderModuleCard(m: ModuleResponse) {
    const group = groupByModule.get(m.id)
    const svcStatus = statusMap[m.id]?.status ?? m.service_status
    const svcKey = normalizeStatus(svcStatus).replace(/\s+/g, '_')
    const canStart = svcKey === 'stopped' || svcKey === 'error' || svcKey === 'not_ready'
    const canStop = ['running', 'starting', 'preparing'].includes(svcKey)
    const { model: activeModel, assumed } = resolveActive(m.id)
    const activeDownload = activeModel
      ? getDownload(m.id, activeModel.model_id)
      : undefined
    const activeDlState = dlState(activeDownload)
    const activeDlActive =
      activeDlState === 'downloading' || activeDlState === 'queued'

    return (
      <Card key={m.id}>
        <CardHeader className="pb-3">
          <div className="flex items-center justify-between gap-2">
            <CardTitle className="flex min-w-0 items-center gap-2 text-base font-semibold">
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
            </CardTitle>
            <ServiceStatusBadge status={svcStatus} />
          </div>
          <CardDescription className="truncate font-mono text-xs">
            {m.id}
            {m.version ? ` · v${m.version}` : ''}
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
              {t('module.logs', { defaultValue: '日志' })}
            </Button>
            <Button size="xs" variant="ghost" onClick={() => setDetailModule(m)}>
              <SlidersHorizontal className="size-3" />
              {t('module.detail', { defaultValue: '详情' })}
            </Button>
            {group ? (
              <Button
                size="xs"
                variant="secondary"
                onClick={() => setRunModule(m)}
                disabled={(m.capabilities ?? []).length === 0}
                title={
                  (m.capabilities ?? []).length === 0
                    ? t('run.noCapabilities', {
                        defaultValue: '该模块未声明任何能力，无法直跑',
                      })
                    : undefined
                }
              >
                <Zap className="size-3" />
                {t('module.run', { defaultValue: '运行' })}
              </Button>
            ) : null}
          </div>

          {group && group.models.length > 0 ? (
            <>
              {/* 变体单槽位选择器（消费 manifest 变体列表 + config.active_models） */}
              <div className="flex flex-wrap items-center gap-2">
                <span className="shrink-0 text-xs text-muted-foreground">
                  {t('card.activeVariant', { defaultValue: '激活变体' })}
                </span>
                <Select
                  value={activeModel?.model_id}
                  onValueChange={(v) => void switchVariant(m.id, v)}
                  disabled={group.models.length <= 1}
                >
                  <SelectTrigger className="h-7 w-auto min-w-36 max-w-full text-xs">
                    <SelectValue
                      placeholder={t('card.selectVariant', {
                        defaultValue: '选择变体',
                      })}
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {group.models.map((variant) => (
                      <SelectItem key={variant.model_id} value={variant.model_id}>
                        {variant.name}
                        {isReadyStatus(variant.status)
                          ? ''
                          : ` (${t(`common:status.${normalizeStatus(variant.status)}`, {
                              defaultValue: variant.status,
                            })})`}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                {assumed ? (
                  <Badge
                    variant="outline"
                    className="px-1.5 text-[10px] font-normal text-muted-foreground"
                    title={t('card.assumedActiveHint', {
                      defaultValue:
                        '尚未显式配置：后端按清单 default 回退解析，切换变体后写入配置',
                    })}
                  >
                    {t('card.assumedActive', { defaultValue: '清单默认' })}
                  </Badge>
                ) : null}
              </div>

              {/* 激活模型投影 */}
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
                    {activeModel.pack_id ? (
                      <Badge
                        variant="outline"
                        className="gap-1 border-border/60 px-1.5 text-[10px] font-normal text-muted-foreground"
                        title={t('card.fromPack', { defaultValue: '来源整合包' })}
                      >
                        <Package className="size-2.5" />
                        {activeModel.pack_id}
                      </Badge>
                    ) : null}
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
                        title={t('card.vramEstimate', { defaultValue: 'VRAM 估算' })}
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
                      <Badge
                        key={tag}
                        variant="secondary"
                        className="cursor-pointer gap-1 pr-1"
                        onClick={() => toggleTagFilter(tag)}
                        title={t('card.tagFilterHint', {
                          defaultValue: '点击按此标签筛选',
                        })}
                      >
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
                        ? t('tags.addFirst', { defaultValue: '添加标签' })
                        : t('common:action.edit')}
                    </Button>
                  </div>

                  {/* 激活变体下载进度（queued/downloading）+ 缺失时的下载入口 */}
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
                              {t('common:action.download')}
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
                          {t('common:action.download')}
                        </Button>
                      )}
                      <span className="text-[11px] text-muted-foreground">
                        {t('card.activeMissingHint', {
                          defaultValue: '激活变体未就绪，下载完成后方可启动模块',
                        })}
                      </span>
                    </div>
                  ) : null}
                </div>
              ) : null}

              {/* 其余变体 */}
              {group.models.filter((x) => x.model_id !== activeModel?.model_id)
                .length > 0 && (
                <div className="divide-y divide-border rounded-lg border border-border">
                  {group.models
                    .filter((x) => x.model_id !== activeModel?.model_id)
                    .map((variant) => renderVariantRow(m.id, variant))}
                </div>
              )}
            </>
          ) : (
            /* 无模型 native 模块：服务卡兜底（§5.1） */
            <p className="rounded-lg border border-dashed border-border px-3 py-2.5 text-xs text-muted-foreground">
              {t('card.serviceFallback', {
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
      description={t('page.description')}
      actions={
        <>
          <Button
            variant="outline"
            size="sm"
            disabled={checkingAll || pageLoading || modelStats.ready === 0}
            onClick={() => void checkAll()}
          >
            {checkingAll ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <ListRestart className="size-3.5" />
            )}
            {t('page.checkAllUpdates')}
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
        {error && (
          <div className="flex items-center gap-2 rounded-lg border border-status-error/30 bg-status-error/10 px-4 py-3 text-sm text-status-error">
            <TriangleAlert className="size-4 shrink-0" />
            <span className="min-w-0 flex-1 truncate">
              {t('page.loadFailed', { error })}
            </span>
            <Button variant="ghost" size="xs" onClick={() => void refresh()}>
              {t('common:action.retry')}
            </Button>
          </div>
        )}

        {/* 缺失模型引导：三条获取途径 */}
        {modelStats.missing > 0 && (
          <div className="flex items-start gap-2.5 rounded-lg border border-status-preparing/30 bg-status-preparing/10 px-4 py-3">
            <CircleAlert className="mt-0.5 size-4 shrink-0 text-status-preparing" />
            <p className="text-xs leading-relaxed text-foreground/80">
              {t('guide.missingModels', { count: modelStats.missing })}
            </p>
          </div>
        )}

        {/* tag chips 筛选（§5.1） */}
        {allTags.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5">
            <Tag className="size-3.5 text-muted-foreground" />
            {allTags.map((tag) => {
              const selected = tagFilters.has(tag)
              return (
                <button
                  key={tag}
                  type="button"
                  onClick={() => toggleTagFilter(tag)}
                  className={cn(
                    'rounded-full border px-2.5 py-0.5 text-xs transition-colors',
                    selected
                      ? 'border-primary bg-primary text-primary-foreground'
                      : 'border-border bg-muted/30 text-muted-foreground hover:bg-muted',
                  )}
                >
                  {tag}
                </button>
              )
            })}
            {tagFilters.size > 0 && (
              <Button
                variant="ghost"
                size="xs"
                onClick={() => setTagFilters(new Set())}
              >
                <X className="size-3" />
                {t('filter.clear', { defaultValue: '清除筛选' })}
              </Button>
            )}
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
              title={t('empty.title')}
              description={t('empty.description')}
              action={{
                label: t('common:action.refresh'),
                onClick: () => {
                  void refresh()
                  void refreshModules()
                },
              }}
            />
          </Card>
        ) : visibleModules.length === 0 ? (
          <Card>
            <EmptyState
              icon={Tag}
              title={t('filter.empty', { defaultValue: '没有匹配该标签筛选的模块' })}
              description={t('filter.emptyDescription', {
                defaultValue: '调整或清除上方标签筛选后重试',
              })}
              action={{
                label: t('filter.clear', { defaultValue: '清除筛选' }),
                onClick: () => setTagFilters(new Set()),
              }}
            />
          </Card>
        ) : (
          <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
            {visibleModules.map((m) => renderModuleCard(m))}
          </div>
        )}

        {/* ── 添加模型：从本机上传 / 服务器本地路径导入 ── */}
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base font-semibold">
              <Upload className="size-4 text-primary" />
              {t('add.title')}
            </CardTitle>
            <CardDescription>{t('add.description')}</CardDescription>
          </CardHeader>
          <CardContent>
            <Tabs defaultValue="upload">
              <TabsList>
                <TabsTrigger value="upload">
                  <Upload className="size-3.5" />
                  {t('upload.tab')}
                </TabsTrigger>
                <TabsTrigger value="import">
                  <HardDrive className="size-3.5" />
                  {t('import.tab')}
                </TabsTrigger>
              </TabsList>

              {/* ── 从本机上传 ── */}
              <TabsContent value="upload" className="mt-4 space-y-4">
                <p className="text-xs text-muted-foreground">
                  {t('upload.description')}
                </p>
                <div className="grid gap-4 sm:grid-cols-2">
                  <div className="space-y-2">
                    <label className="text-sm font-medium">
                      {t('add.targetModule')}
                    </label>
                    <Select
                      value={uploadModule || undefined}
                      onValueChange={(v) => {
                        setUploadModule(v)
                        setUploadModelId('')
                      }}
                      disabled={!!uploading}
                    >
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder={t('add.selectModule')} />
                      </SelectTrigger>
                      <SelectContent>
                        {moduleOptions.map((mo) => (
                          <SelectItem key={mo.module_id} value={mo.module_id}>
                            {mo.module_name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-2">
                    <label className="text-sm font-medium">
                      {t('common:label.model')}
                    </label>
                    <Select
                      value={uploadModelId || undefined}
                      onValueChange={setUploadModelId}
                      disabled={!uploadModule || !!uploading}
                    >
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder={t('add.selectModel')} />
                      </SelectTrigger>
                      <SelectContent>
                        {uploadModelOptions.map((mo) => (
                          <SelectItem key={mo.model_id} value={mo.model_id}>
                            {mo.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                </div>

                {/* 拖拽区：文件夹 / 压缩包同一入口 */}
                <div
                  onDragOver={(e) => {
                    e.preventDefault()
                    if (!uploading && !collecting) setDragOver(true)
                  }}
                  onDragLeave={(e) => {
                    // 仅在真正离开拖拽区（而非移入子元素）时取消高亮
                    if (!e.currentTarget.contains(e.relatedTarget as Node)) {
                      setDragOver(false)
                    }
                  }}
                  onDrop={(e) => void handleDrop(e)}
                  className={cn(
                    'rounded-lg border-2 border-dashed px-6 py-8 text-center transition-colors',
                    dragOver
                      ? 'border-primary bg-primary/5'
                      : 'border-border bg-muted/20',
                  )}
                >
                  {uploading ? (
                    <div className="mx-auto max-w-md space-y-3">
                      <div className="flex items-center justify-between font-mono text-xs text-muted-foreground">
                        <span>
                          {uploading.percent >= 100
                            ? t('upload.transferDoneProcessing')
                            : t('upload.uploadingPercent', {
                                percent: Math.floor(uploading.percent),
                              })}
                        </span>
                        <span>
                          {formatBytes(uploading.loaded)} /{' '}
                          {formatBytes(uploading.total)}
                        </span>
                      </div>
                      <Progress value={uploading.percent} />
                      <Button
                        variant="outline"
                        size="xs"
                        onClick={() => abortUploadRef.current?.()}
                      >
                        <X className="size-3" />
                        {t('upload.cancelUpload')}
                      </Button>
                    </div>
                  ) : collecting ? (
                    <div className="flex flex-col items-center gap-2">
                      <Loader2 className="size-6 animate-spin text-muted-foreground" />
                      <p className="text-sm text-muted-foreground">
                        {t('upload.readingFiles')}
                      </p>
                    </div>
                  ) : picked ? (
                    <div className="mx-auto flex max-w-lg items-center justify-between gap-3 rounded-md border border-border bg-background px-4 py-3 text-left">
                      <div className="flex min-w-0 items-center gap-2.5">
                        {picked.mode === 'archive' ? (
                          <FileArchive className="size-5 shrink-0 text-muted-foreground" />
                        ) : (
                          <FolderUp className="size-5 shrink-0 text-muted-foreground" />
                        )}
                        <div className="min-w-0">
                          <div className="flex items-center gap-2">
                            <span className="truncate text-sm font-medium">
                              {picked.mode === 'archive'
                                ? picked.files[0].name
                                : t('upload.folderUpload')}
                            </span>
                            <Badge
                              variant="secondary"
                              className="font-mono text-[10px] text-muted-foreground"
                            >
                              {t('upload.fileCount', {
                                count: picked.files.length,
                              })}
                            </Badge>
                          </div>
                          <div className="font-mono text-xs text-muted-foreground">
                            {formatBytes(picked.totalBytes)}
                            {picked.mode === 'archive' &&
                              ` · ${t('upload.extractedOnServer')}`}
                          </div>
                        </div>
                      </div>
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        title={t('upload.clearSelection')}
                        onClick={() => setPicked(null)}
                      >
                        <X className="size-3.5" />
                      </Button>
                    </div>
                  ) : (
                    <div className="flex flex-col items-center gap-2">
                      <Upload className="size-6 text-muted-foreground/60" />
                      <p className="text-sm">{t('upload.dropHint')}</p>
                      <p className="text-xs text-muted-foreground">
                        {t('upload.dropHintSub')}
                      </p>
                      <div className="mt-2 flex flex-wrap justify-center gap-2">
                        <Button
                          variant="outline"
                          size="sm"
                          type="button"
                          onClick={() => folderInputRef.current?.click()}
                        >
                          <FolderUp className="size-3.5" />
                          {t('upload.pickFolder')}
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          type="button"
                          onClick={() => archiveInputRef.current?.click()}
                        >
                          <FileArchive className="size-3.5" />
                          {t('upload.pickArchive')}
                        </Button>
                      </div>
                    </div>
                  )}
                </div>

                {/* 隐藏的文件夹选择输入：webkitdirectory 无 TS 类型，用 setAttribute 设置 */}
                <input
                  ref={(el) => {
                    folderInputRef.current = el
                    if (el) el.setAttribute('webkitdirectory', '')
                  }}
                  type="file"
                  multiple
                  className="hidden"
                  onChange={(e) => {
                    handleFilesPicked(Array.from(e.target.files ?? []))
                    e.target.value = ''
                  }}
                />
                {/* 隐藏的压缩包选择输入 */}
                <input
                  ref={archiveInputRef}
                  type="file"
                  accept=".zip,.tar.gz,.tgz"
                  className="hidden"
                  onChange={(e) => {
                    handleFilesPicked(Array.from(e.target.files ?? []))
                    e.target.value = ''
                  }}
                />

                <div className="flex items-center justify-between gap-3">
                  <p className="text-xs text-muted-foreground">
                    {picked
                      ? t('upload.pickedHint', {
                          count: picked.files.length,
                          size: formatBytes(picked.totalBytes),
                        })
                      : t('upload.existsHint')}
                  </p>
                  <Button
                    disabled={
                      !uploadModule ||
                      !uploadModelId ||
                      !picked ||
                      !!uploading ||
                      collecting
                    }
                    onClick={() => void handleUpload()}
                  >
                    {uploading ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <Upload className="size-4" />
                    )}
                    {t('upload.start')}
                  </Button>
                </div>
              </TabsContent>

              {/* ── 服务器本地路径导入 ── */}
              <TabsContent value="import" className="mt-4 space-y-4">
                <p className="text-xs text-muted-foreground">
                  {t('import.description')}
                </p>
                <div className="grid items-end gap-4 sm:grid-cols-2 lg:grid-cols-[1fr_1fr_1.6fr_auto]">
                  <div className="space-y-2">
                    <label className="text-sm font-medium">
                      {t('add.targetModule')}
                    </label>
                    <Select
                      value={importModule || undefined}
                      onValueChange={(v) => {
                        setImportModule(v)
                        setImportModelId('')
                      }}
                    >
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder={t('add.selectModule')} />
                      </SelectTrigger>
                      <SelectContent>
                        {moduleOptions.map((mo) => (
                          <SelectItem key={mo.module_id} value={mo.module_id}>
                            {mo.module_name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-2">
                    <label className="text-sm font-medium">
                      {t('common:label.model')}
                    </label>
                    <Select
                      value={importModelId || undefined}
                      onValueChange={setImportModelId}
                      disabled={!importModule}
                    >
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder={t('add.selectModel')} />
                      </SelectTrigger>
                      <SelectContent>
                        {importModelOptions.map((mo) => (
                          <SelectItem key={mo.model_id} value={mo.model_id}>
                            {mo.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-2">
                    <label className="text-sm font-medium">
                      {t('import.sourcePath')}
                    </label>
                    <Input
                      value={sourcePath}
                      onChange={(e) => setSourcePath(e.target.value)}
                      placeholder="/path/to/model/files"
                      className="font-mono text-xs"
                    />
                  </div>
                  <Button
                    onClick={() => void handleImport()}
                    disabled={
                      importing ||
                      !importModule ||
                      !importModelId ||
                      !sourcePath.trim()
                    }
                  >
                    {importing ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <Download className="size-4" />
                    )}
                    {t('common:action.import')}
                  </Button>
                </div>
              </TabsContent>
            </Tabs>
          </CardContent>
        </Card>
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

      {/* ── 停止模块确认 ── */}
      <ConfirmDialog
        open={stopTarget !== null}
        onOpenChange={(open) => {
          if (!open) setStopTarget(null)
        }}
        variant="destructive"
        title={t('module.stopConfirmTitle', {
          defaultValue: '停止模块「{{name}}」？',
          name: stopTarget?.name ?? '',
        })}
        description={t('module.stopConfirmDescription', {
          defaultValue: '停止后正在执行的任务可能失败，需重新启动模块才能再次运行',
        })}
        confirmLabel={t('common:action.stop')}
        onConfirm={() => stopModuleConfirmed()}
      />

      {/* ── 删除模型确认 ── */}
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
        variant="destructive"
        title={t('delete.confirmTitle', {
          name: deleteTarget?.model.name ?? '',
        })}
        description={t('delete.confirmDescription')}
        confirmLabel={t('common:action.delete')}
        onConfirm={() => confirmDelete()}
      />
    </PageContainer>
  )
}
