import { useMemo, useRef, useState, type DragEvent } from 'react'
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
  HardDrive,
  ListRestart,
  Loader2,
  RefreshCw,
  Sparkles,
  Trash2,
  TriangleAlert,
  Upload,
  X,
} from 'lucide-react'
import { toast } from 'sonner'
import { api } from '@/api/client'
import type { ModelInfo, ModelSource } from '@/api/types'
import { PageContainer } from '@/components/layout/page-container'
import { ConfirmDialog } from '@/components/shared/confirm-dialog'
import { EmptyState } from '@/components/shared/empty-state'
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
import { Skeleton } from '@/components/ui/skeleton'
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs'
import { useModels } from '@/hooks/use-models'
import {
  useModelDownloads,
  type ModelDownloadProgress,
} from '@/hooks/use-model-downloads'
import { cn, formatBytes, formatMB } from '@/lib/utils'

// ─── 常量与工具 ──────────────────────────────────────────────────────────────

/** 模型来源 → 展示标签（中文语境下保留平台名） */
const SOURCE_LABELS: Record<string, string> = {
  huggingface: 'HuggingFace',
  modelscope: 'ModelScope',
  url: 'URL 直链',
  local_import: '本地导入',
}

function sourceLabel(source: string): string {
  return SOURCE_LABELS[source.toLowerCase()] ?? source
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
  if (!(e instanceof Error)) return String(e)
  const m = e.message.match(/^API \d+: (.*)$/s)
  const body = m ? m[1] : e.message
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
            : `API ${xhr.status}: 上传失败`,
        ),
      )
    })
    xhr.addEventListener('error', () => reject(new Error('网络错误，上传失败')))
    xhr.addEventListener('abort', () => reject(new Error('上传已取消')))
    xhr.send(form)
  })
  return { promise, abort: () => xhr.abort() }
}

// ─── 状态徽章 ────────────────────────────────────────────────────────────────

interface ModelStatusMeta {
  label: string
  dot: string
  badge: string
  transitional: boolean
}

/** 模型状态 → 元信息（ready=绿 / missing=红 / incomplete=黄 / downloading=蓝） */
function modelStatusMeta(status: string): ModelStatusMeta {
  switch (status.trim().toLowerCase()) {
    case 'ready':
      return {
        label: '就绪',
        dot: 'bg-status-running',
        badge: 'bg-status-running/15 text-status-running border-status-running/30',
        transitional: false,
      }
    case 'missing':
      return {
        label: '缺失',
        dot: 'bg-status-error',
        badge: 'bg-status-error/15 text-status-error border-status-error/30',
        transitional: false,
      }
    case 'incomplete':
      return {
        label: '不完整',
        dot: 'bg-status-preparing',
        badge:
          'bg-status-preparing/15 text-status-preparing border-status-preparing/30',
        transitional: false,
      }
    case 'downloading':
      return {
        label: '下载中',
        dot: 'bg-status-starting',
        badge:
          'bg-status-starting/15 text-status-starting border-status-starting/30',
        transitional: true,
      }
    default:
      return {
        label: status,
        dot: 'bg-muted-foreground',
        badge: 'bg-muted text-muted-foreground border-border',
        transitional: false,
      }
  }
}

function ModelStatusBadge({ status }: { status: string }) {
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
      {meta.label}
    </Badge>
  )
}

function SourceBadges({ model }: { model: ModelInfo }) {
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
          {sourceLabel(s)}
        </Badge>
      ))}
    </>
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

// ─── 页面 ────────────────────────────────────────────────────────────────────

export function ModelsPage() {
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

  /** 已展开的模型行（key = module_id/model_id） */
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [detailLoading, setDetailLoading] = useState<Record<string, boolean>>(
    {},
  )

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

  // ── 删除确认 ──
  const [deleteTarget, setDeleteTarget] = useState<{
    moduleId: string
    model: ModelInfo
  } | null>(null)

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
      toast.success('模型下载完成', { description: name })
    } else if (entry.state === 'failed') {
      toast.error('模型下载失败', {
        description: `${name}，可重试或更换下载源`,
      })
    } else {
      toast.info('模型下载已取消', { description: name })
    }
  }

  const { get: getDownload, refresh: refreshDownloads } =
    useModelDownloads(handleDownloadSettled)

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
      toast.success('已开始下载', {
        description: `${model.name}${chosen ? ` · ${sourceLabel(chosen)}` : ''}`,
      })
      // 立即拉一次下载列表，保证进度条尽快出现（WS 初始推送可能稍晚）
      void refreshDownloads()
    } catch (e) {
      toast.error('下载启动失败', { description: errMsg(e) })
    }
  }

  // ── 删除 ──
  async function confirmDelete() {
    if (!deleteTarget) return
    try {
      await deleteModel(deleteTarget.moduleId, deleteTarget.model.model_id)
      toast.success('模型已删除', { description: deleteTarget.model.name })
      const key = `${deleteTarget.moduleId}/${deleteTarget.model.model_id}`
      setUpdateInfo((prev) => {
        if (!(key in prev)) return prev
        const next = { ...prev }
        delete next[key]
        return next
      })
    } catch (e) {
      toast.error('删除模型失败', { description: errMsg(e) })
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
        toast.info(`「${model.name}」有可用更新`, { description: result.reason })
      } else {
        toast.success(`「${model.name}」已是最新版本`, {
          description: result.reason,
        })
      }
    } catch (e) {
      toast.error('检查更新失败', { description: errMsg(e) })
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
      toast.info('没有可检查的模型', { description: '仅就绪状态的模型支持检查更新' })
      return
    }
    setCheckingAll(true)
    try {
      const results = await Promise.allSettled(
        targets.map((t) => api.checkModelUpdate(t.moduleId, t.model.model_id)),
      )
      const patch: Record<string, UpdateCheckResult> = {}
      const updatable: string[] = []
      let failed = 0
      results.forEach((r, i) => {
        const t = targets[i]
        const key = `${t.moduleId}/${t.model.model_id}`
        if (r.status === 'fulfilled') {
          patch[key] = r.value
          if (r.value.available) updatable.push(t.model.name)
        } else {
          failed++
        }
      })
      setUpdateInfo((prev) => ({ ...prev, ...patch }))
      if (updatable.length > 0) {
        toast.warning(`${updatable.length} 个模型有可用更新`, {
          description: updatable.join('、'),
        })
      } else if (failed > 0) {
        toast.error(`${failed} 个模型检查失败，其余模型均为最新`)
      } else {
        toast.success('全部模型均为最新版本')
      }
    } finally {
      setCheckingAll(false)
    }
  }

  // ── 服务器本地路径导入 ──
  async function handleImport() {
    if (!importModule || !importModelId || !sourcePath.trim()) return
    setImporting(true)
    const toastId = toast.loading('正在导入模型…')
    try {
      const resp = await importModel(importModule, {
        model_id: importModelId,
        source_path: sourcePath.trim(),
      })
      if (resp.error) {
        toast.error('导入失败', { id: toastId, description: resp.error })
      } else {
        toast.success('模型导入成功', {
          id: toastId,
          description: `${resp.file_count ?? '–'} 个文件 · ${formatBytes(resp.total_bytes)}`,
        })
        setImportModelId('')
        setSourcePath('')
      }
    } catch (e) {
      toast.error('导入失败', {
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
        toast.error('未从拖拽内容中读取到文件')
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
      toast.error('读取拖拽内容失败', { description: errMsg(err) })
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
    const toastId = toast.loading('正在上传模型文件…')
    const { promise, abort } = uploadModelWithProgress(
      moduleId,
      uploadModelId,
      files,
      paths,
      setUploading,
    )
    abortUploadRef.current = abort
    try {
      await promise
      toast.success('模型上传成功', {
        id: toastId,
        description: `${files.length} 个文件 · ${formatBytes(totalBytes)}`,
      })
      setPicked(null)
      setUploadModelId('')
      // 刷新列表；该模块详情已展开则一并同步
      await Promise.allSettled([
        refresh(),
        details[moduleId] ? moduleModels(moduleId) : Promise.resolve(),
      ])
    } catch (e) {
      if (e instanceof Error && e.message === '上传已取消') {
        toast.info('上传已取消', { id: toastId })
      } else {
        toast.error('模型上传失败', { id: toastId, description: errMsg(e) })
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
                `获取模型详情失败：${e instanceof Error ? e.message : String(e)}`,
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

  const moduleOptions = models?.modules ?? []
  const uploadModelOptions =
    moduleOptions.find((m) => m.module_id === uploadModule)?.models ?? []
  const importModelOptions =
    moduleOptions.find((m) => m.module_id === importModule)?.models ?? []

  return (
    <PageContainer
      title="模型管理"
      description="按模块查看模型状态，支持在线下载、从本机上传与服务器本地路径导入"
      actions={
        <>
          <Button
            variant="outline"
            size="sm"
            disabled={checkingAll || loading || modelStats.ready === 0}
            onClick={() => void checkAll()}
          >
            {checkingAll ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <ListRestart className="size-3.5" />
            )}
            检查全部更新
          </Button>
          <Button variant="outline" size="sm" onClick={() => void refresh()}>
            <RefreshCw className="size-3.5" />
            刷新
          </Button>
        </>
      }
    >
      <div className="space-y-6">
        {error && (
          <div className="flex items-center gap-2 rounded-lg border border-status-error/30 bg-status-error/10 px-4 py-3 text-sm text-status-error">
            <TriangleAlert className="size-4 shrink-0" />
            <span className="min-w-0 flex-1 truncate">加载失败：{error}</span>
            <Button variant="ghost" size="xs" onClick={() => void refresh()}>
              重试
            </Button>
          </div>
        )}

        {/* 缺失模型引导：三条获取途径 */}
        {modelStats.missing > 0 && (
          <div className="flex items-start gap-2.5 rounded-lg border border-status-preparing/30 bg-status-preparing/10 px-4 py-3">
            <CircleAlert className="mt-0.5 size-4 shrink-0 text-status-preparing" />
            <p className="text-xs leading-relaxed text-foreground/80">
              有 {modelStats.missing}{' '}
              个模型缺失或不完整，可通过三种途径获取：① 点击列表中的「下载」在线获取；②
              使用下方「从本机上传」上传浏览器中的模型文件；③
              手动将文件复制到服务器后，用「服务器本地路径导入」添加。
            </p>
          </div>
        )}

        {loading ? (
          <div className="space-y-4">
            {Array.from({ length: 2 }).map((_, i) => (
              <Skeleton key={i} className="h-40 rounded-lg" />
            ))}
          </div>
        ) : !models || models.modules.length === 0 || modelStats.total === 0 ? (
          <Card>
            <EmptyState
              icon={Database}
              title="暂无模型信息"
              description="安装模块后，其所需模型将在此处显示。获取模型有三种途径：① 在线下载——在模型列表中点击「下载」；② 从本机上传——把浏览器中的模型文件夹或压缩包上传到服务器；③ 手动将模型文件复制到服务器，再用下方「服务器本地路径导入」添加。"
              action={{ label: '刷新', onClick: () => void refresh() }}
            />
          </Card>
        ) : (
          models.modules.map((group) => (
            <Card key={group.module_id}>
              <CardHeader className="pb-3">
                <div className="flex items-center justify-between gap-2">
                  <CardTitle className="flex items-center gap-2 text-base font-semibold">
                    <Database className="size-4 text-primary" />
                    {group.module_name}
                    <Badge
                      variant="secondary"
                      className="font-mono text-xs text-muted-foreground"
                    >
                      {group.models.length}
                    </Badge>
                  </CardTitle>
                  <span className="font-mono text-xs text-muted-foreground">
                    {group.module_id}
                  </span>
                </div>
              </CardHeader>
              <CardContent className="p-0">
                {group.models.length === 0 ? (
                  <p className="px-6 pb-5 text-sm text-muted-foreground">
                    该模块暂无关联模型
                  </p>
                ) : (
                  <div className="divide-y divide-border">
                    {group.models.map((model) => {
                      const key = `${group.module_id}/${model.model_id}`
                      const isOpen = expanded.has(key)
                      const detail = details[group.module_id]?.models.find(
                        (d) => d.model_id === model.model_id,
                      )
                      const download = getDownload(
                        group.module_id,
                        model.model_id,
                      )
                      const isDownloading = download?.state === 'downloading'
                      const status = normalizeStatus(model.status)
                      const canDownload =
                        (status === 'missing' || status === 'incomplete') &&
                        !isDownloading
                      const sources = model.available_sources ?? []
                      const update = updateInfo[key]
                      return (
                        <div key={model.model_id}>
                          <div
                            role="button"
                            tabIndex={0}
                            onClick={() =>
                              void toggleExpand(group.module_id, model)
                            }
                            onKeyDown={(e) => {
                              if (e.key === 'Enter' || e.key === ' ') {
                                e.preventDefault()
                                void toggleExpand(group.module_id, model)
                              }
                            }}
                            className="flex w-full cursor-pointer flex-wrap items-center gap-x-3 gap-y-2 px-6 py-3.5 text-left transition-colors hover:bg-muted/50"
                          >
                            <ChevronRight
                              className={cn(
                                'size-4 shrink-0 text-muted-foreground transition-transform',
                                isOpen && 'rotate-90',
                              )}
                            />
                            <div className="min-w-0 flex-1">
                              <div className="flex min-w-0 items-center gap-2">
                                <span className="truncate text-sm font-medium">
                                  {model.name}
                                </span>
                                {update?.available && (
                                  <Badge
                                    variant="outline"
                                    className="border-status-preparing/30 bg-status-preparing/15 px-1.5 text-[10px] text-status-preparing"
                                  >
                                    <Sparkles className="size-2.5" />
                                    有更新
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

                            <div
                              className="flex shrink-0 flex-wrap items-center justify-end gap-2"
                              onClick={(e) => e.stopPropagation()}
                            >
                              <ModelStatusBadge
                                status={
                                  isDownloading ? 'downloading' : model.status
                                }
                              />
                              {isDownloading && download ? (
                                <div className="w-40 sm:w-52">
                                  <div className="mb-1 flex items-center justify-between font-mono text-[11px] text-muted-foreground">
                                    <span>
                                      {Math.floor(download.percent)}%
                                    </span>
                                    <span>{formatBytes(download.bytes)}</span>
                                  </div>
                                  <Progress
                                    value={download.percent}
                                    className="h-1.5"
                                  />
                                </div>
                              ) : (
                                <span className="hidden w-20 text-right font-mono text-xs text-muted-foreground sm:block">
                                  {formatMB(model.size_estimate_mb)}
                                </span>
                              )}

                              {/* 下载：多源先选源，单源直接下载 */}
                              {canDownload &&
                                (sources.length > 1 ? (
                                  <DropdownMenu>
                                    <DropdownMenuTrigger asChild>
                                      <Button
                                        size="xs"
                                        onClick={(e) => e.stopPropagation()}
                                      >
                                        <Download className="size-3" />
                                        下载
                                        <ChevronDown className="size-3" />
                                      </Button>
                                    </DropdownMenuTrigger>
                                    <DropdownMenuContent align="end">
                                      {sources.map((s) => (
                                        <DropdownMenuItem
                                          key={s}
                                          onSelect={() =>
                                            void startDownload(
                                              group.module_id,
                                              model,
                                              s,
                                            )
                                          }
                                        >
                                          <Download className="size-3.5" />
                                          {sourceLabel(s)}
                                        </DropdownMenuItem>
                                      ))}
                                    </DropdownMenuContent>
                                  </DropdownMenu>
                                ) : (
                                  <Button
                                    size="xs"
                                    onClick={(e) => {
                                      e.stopPropagation()
                                      void startDownload(
                                        group.module_id,
                                        model,
                                        sources[0],
                                      )
                                    }}
                                  >
                                    <Download className="size-3" />
                                    下载
                                  </Button>
                                ))}

                              {/* 有更新：重新下载（走主源下载流程） */}
                              {update?.available && !isDownloading && (
                                <Button
                                  size="xs"
                                  variant="outline"
                                  onClick={(e) => {
                                    e.stopPropagation()
                                    void startDownload(
                                      group.module_id,
                                      model,
                                      primarySource(model),
                                    )
                                  }}
                                >
                                  <Download className="size-3" />
                                  重新下载
                                </Button>
                              )}

                              {/* 就绪模型：检查更新 + 删除 */}
                              {status === 'ready' && (
                                <>
                                  <Button
                                    size="xs"
                                    variant="ghost"
                                    disabled={checking.has(key) || checkingAll}
                                    onClick={(e) => {
                                      e.stopPropagation()
                                      void checkOne(group.module_id, model)
                                    }}
                                  >
                                    {checking.has(key) ? (
                                      <Loader2 className="size-3 animate-spin" />
                                    ) : (
                                      <Sparkles className="size-3" />
                                    )}
                                    检查更新
                                  </Button>
                                  <Button
                                    size="xs"
                                    variant="ghost"
                                    className="text-muted-foreground hover:text-destructive"
                                    onClick={(e) => {
                                      e.stopPropagation()
                                      setDeleteTarget({
                                        moduleId: group.module_id,
                                        model,
                                      })
                                    }}
                                  >
                                    <Trash2 className="size-3" />
                                    删除
                                  </Button>
                                </>
                              )}
                            </div>
                          </div>
                          {isOpen && (
                            <div className="border-t border-dashed border-border bg-muted/30 px-6 py-4 pl-13">
                              {detailLoading[group.module_id] && !detail ? (
                                <div className="space-y-2">
                                  <Skeleton className="h-4 w-48" />
                                  <Skeleton className="h-4 w-64" />
                                </div>
                              ) : detail ? (
                                <dl className="grid gap-x-8 gap-y-2 text-sm sm:grid-cols-3">
                                  <div>
                                    <dt className="flex items-center gap-1.5 text-xs text-muted-foreground">
                                      <FileBox className="size-3.5" />
                                      文件数
                                    </dt>
                                    <dd className="mt-1 font-mono">
                                      {detail.file_count ?? '–'}
                                    </dd>
                                  </div>
                                  <div>
                                    <dt className="flex items-center gap-1.5 text-xs text-muted-foreground">
                                      <HardDrive className="size-3.5" />
                                      实际大小
                                    </dt>
                                    <dd className="mt-1 font-mono">
                                      {formatBytes(detail.size_bytes)}
                                    </dd>
                                  </div>
                                  <div className="min-w-0">
                                    <dt className="flex items-center gap-1.5 text-xs text-muted-foreground">
                                      <FolderOpen className="size-3.5" />
                                      本地缓存路径
                                    </dt>
                                    <dd className="mt-1 break-all font-mono text-xs leading-relaxed">
                                      {detail.local_cache_path ??
                                        detail.target_dir}
                                    </dd>
                                  </div>
                                </dl>
                              ) : (
                                <p className="text-xs text-muted-foreground">
                                  暂无详情数据
                                </p>
                              )}
                            </div>
                          )}
                        </div>
                      )
                    })}
                  </div>
                )}
              </CardContent>
            </Card>
          ))
        )}

        {/* ── 添加模型：从本机上传 / 服务器本地路径导入 ── */}
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base font-semibold">
              <Upload className="size-4 text-primary" />
              添加模型
            </CardTitle>
            <CardDescription>
              在线下载请直接使用上方列表中的「下载」按钮；此处提供另外两种途径
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Tabs defaultValue="upload">
              <TabsList>
                <TabsTrigger value="upload">
                  <Upload className="size-3.5" />
                  从本机上传
                </TabsTrigger>
                <TabsTrigger value="import">
                  <HardDrive className="size-3.5" />
                  服务器本地路径导入
                </TabsTrigger>
              </TabsList>

              {/* ── 从本机上传 ── */}
              <TabsContent value="upload" className="mt-4 space-y-4">
                <p className="text-xs text-muted-foreground">
                  将你自己电脑上的模型文件（浏览器端文件）上传到服务器：支持整个文件夹，或单个
                  .zip / .tar.gz / .tgz 压缩包（由服务端解包）。
                </p>
                <div className="grid gap-4 sm:grid-cols-2">
                  <div className="space-y-2">
                    <label className="text-sm font-medium">目标模块</label>
                    <Select
                      value={uploadModule || undefined}
                      onValueChange={(v) => {
                        setUploadModule(v)
                        setUploadModelId('')
                      }}
                      disabled={!!uploading}
                    >
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder="选择模块" />
                      </SelectTrigger>
                      <SelectContent>
                        {moduleOptions.map((m) => (
                          <SelectItem key={m.module_id} value={m.module_id}>
                            {m.module_name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-2">
                    <label className="text-sm font-medium">模型</label>
                    <Select
                      value={uploadModelId || undefined}
                      onValueChange={setUploadModelId}
                      disabled={!uploadModule || !!uploading}
                    >
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder="选择模型" />
                      </SelectTrigger>
                      <SelectContent>
                        {uploadModelOptions.map((m) => (
                          <SelectItem key={m.model_id} value={m.model_id}>
                            {m.name}
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
                            ? '传输完成，服务器处理中…'
                            : `正在上传 ${Math.floor(uploading.percent)}%`}
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
                        取消上传
                      </Button>
                    </div>
                  ) : collecting ? (
                    <div className="flex flex-col items-center gap-2">
                      <Loader2 className="size-6 animate-spin text-muted-foreground" />
                      <p className="text-sm text-muted-foreground">
                        正在读取文件…
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
                                : '文件夹上传'}
                            </span>
                            <Badge
                              variant="secondary"
                              className="font-mono text-[10px] text-muted-foreground"
                            >
                              {picked.files.length} 个文件
                            </Badge>
                          </div>
                          <div className="font-mono text-xs text-muted-foreground">
                            {formatBytes(picked.totalBytes)}
                            {picked.mode === 'archive' && ' · 服务端解包'}
                          </div>
                        </div>
                      </div>
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        title="清除已选文件"
                        onClick={() => setPicked(null)}
                      >
                        <X className="size-3.5" />
                      </Button>
                    </div>
                  ) : (
                    <div className="flex flex-col items-center gap-2">
                      <Upload className="size-6 text-muted-foreground/60" />
                      <p className="text-sm">
                        拖拽模型文件夹或 .zip / .tar.gz / .tgz 压缩包到此处
                      </p>
                      <p className="text-xs text-muted-foreground">
                        文件夹按相对路径保留目录结构；压缩包由服务端自动解包
                      </p>
                      <div className="mt-2 flex flex-wrap justify-center gap-2">
                        <Button
                          variant="outline"
                          size="sm"
                          type="button"
                          onClick={() => folderInputRef.current?.click()}
                        >
                          <FolderUp className="size-3.5" />
                          选择文件夹
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          type="button"
                          onClick={() => archiveInputRef.current?.click()}
                        >
                          <FileArchive className="size-3.5" />
                          选择压缩包
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
                      ? `将上传 ${picked.files.length} 个文件（${formatBytes(picked.totalBytes)}），大文件耗时较长，请保持页面打开`
                      : '目标模型已存在时上传会被拒绝，需先删除旧模型'}
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
                    开始上传
                  </Button>
                </div>
              </TabsContent>

              {/* ── 服务器本地路径导入 ── */}
              <TabsContent value="import" className="mt-4 space-y-4">
                <p className="text-xs text-muted-foreground">
                  模型文件已在服务器上时使用：填写包含模型文件的目录路径，后端将其复制到模块模型缓存目录，跳过网络下载。
                </p>
                <div className="grid items-end gap-4 sm:grid-cols-2 lg:grid-cols-[1fr_1fr_1.6fr_auto]">
                  <div className="space-y-2">
                    <label className="text-sm font-medium">目标模块</label>
                    <Select
                      value={importModule || undefined}
                      onValueChange={(v) => {
                        setImportModule(v)
                        setImportModelId('')
                      }}
                    >
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder="选择模块" />
                      </SelectTrigger>
                      <SelectContent>
                        {moduleOptions.map((m) => (
                          <SelectItem key={m.module_id} value={m.module_id}>
                            {m.module_name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-2">
                    <label className="text-sm font-medium">模型</label>
                    <Select
                      value={importModelId || undefined}
                      onValueChange={setImportModelId}
                      disabled={!importModule}
                    >
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder="选择模型" />
                      </SelectTrigger>
                      <SelectContent>
                        {importModelOptions.map((m) => (
                          <SelectItem key={m.model_id} value={m.model_id}>
                            {m.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-2">
                    <label className="text-sm font-medium">源路径</label>
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
                    导入
                  </Button>
                </div>
              </TabsContent>
            </Tabs>
          </CardContent>
        </Card>
      </div>

      {/* ── 删除模型确认 ── */}
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
        variant="destructive"
        title={`删除模型「${deleteTarget?.model.name ?? ''}」？`}
        description="将删除服务器上的模型文件（整个模型目录及元数据）。此操作不可撤销，之后需要重新下载或上传才能恢复。"
        confirmLabel="删除"
        onConfirm={() => confirmDelete()}
      />
    </PageContainer>
  )
}
