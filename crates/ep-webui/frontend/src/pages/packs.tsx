import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  CircleCheck,
  CircleX,
  Download,
  FileArchive,
  Globe,
  HardDrive,
  Info,
  Loader2,
  PackageOpen,
  PackagePlus,
  RefreshCw,
  Trash2,
  TriangleAlert,
  Upload,
  X,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { api } from '@/api/client'
import type {
  ModelInfo,
  PackDetail,
  PackImportResponse,
  PackInfo,
  PipelineSummary,
} from '@/api/types'
import { wsManager } from '@/api/ws'
import { PageContainer } from '@/components/layout/page-container'
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
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Progress } from '@/components/ui/progress'
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
import { cn, formatBytes } from '@/lib/utils'

// ─── 工具 ────────────────────────────────────────────────────────────────────

/** 从 `API <status>: <body>` 错误中提取可读文案（后端错误体 {"error":"..."}） */
function errMsg(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e)
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

/** pack id 语法（§4.2 `<publisher>.<pack-name>`，两段 lowercase 字母数字-） */
const PACK_ID_PATTERN = /^[a-z0-9][a-z0-9-]*\.[a-z0-9][a-z0-9-]*$/

/** ISO 时间 → 本地化展示 */
function formatInstalledAt(iso: string | undefined): string {
  if (!iso) return ''
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return date.toLocaleString()
}

// ─── WS 导入/构建进度 ────────────────────────────────────────────────────────

/** 单个 pack 的导入/构建进度（WS pack_import 聚合） */
interface PackProgressEntry {
  stage?: string
  percent?: number
  /** running / completed / failed */
  state: string
  message?: string
}

// ─── 上传（XHR 真实进度）────────────────────────────────────────────────────

/**
 * XHR 上传 .epzip（整合包可达数 GB，进度反馈必需；fetch 无上传进度）。
 * 表单字段名 `file`（仲裁 #3 统一约定）。
 */
function uploadPackWithProgress(
  file: File,
  onProgress: (percent: number, loaded: number, total: number) => void,
): { promise: Promise<PackImportResponse>; abort: () => void } {
  const form = new FormData()
  form.append('file', file)
  const xhr = new XMLHttpRequest()
  const promise = new Promise<PackImportResponse>((resolve, reject) => {
    xhr.open('POST', '/api/packs/upload')
    xhr.upload.addEventListener('progress', (e) => {
      if (!e.lengthComputable) return
      onProgress(
        e.total > 0 ? Math.min(100, (e.loaded / e.total) * 100) : 0,
        e.loaded,
        e.total,
      )
    })
    xhr.addEventListener('load', () => {
      let body: unknown = null
      try {
        body = JSON.parse(xhr.responseText)
      } catch {
        // 非 JSON 响应
      }
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve((body ?? {}) as PackImportResponse)
        return
      }
      const msg =
        body && typeof body === 'object' && 'error' in body
          ? (body as { error: unknown }).error
          : null
      reject(
        new Error(typeof msg === 'string' && msg.trim() ? msg : `HTTP ${xhr.status}`),
      )
    })
    xhr.addEventListener('error', () => reject(new Error('network error')))
    xhr.addEventListener('abort', () => reject(new Error('__ep_pack_upload_aborted__')))
    xhr.send(form)
  })
  return { promise, abort: () => xhr.abort() }
}

// ─── 页面 ────────────────────────────────────────────────────────────────────

export function PacksPage() {
  const { t } = useTranslation('packs')
  const [packs, setPacks] = useState<PackInfo[] | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  /** 导入/构建进度表（pack_id → WS 聚合状态；completed/failed 保留至手动关闭） */
  const [progress, setProgress] = useState<Record<string, PackProgressEntry>>({})

  // ── 对话框 / 抽屉状态 ──
  const [importOpen, setImportOpen] = useState(false)
  const [buildOpen, setBuildOpen] = useState(false)
  const [detailId, setDetailId] = useState<string | null>(null)
  const [uninstallTarget, setUninstallTarget] = useState<PackInfo | null>(null)
  const [keepModels, setKeepModels] = useState(true)

  const loadPacks = useCallback(async () => {
    try {
      setError(null)
      setPacks(await api.listPacks())
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void loadPacks()
  }, [loadPacks])

  // WS pack_import 订阅：进度聚合 + 终态 toast/刷新
  useEffect(() => {
    return wsManager.onMessage((msg) => {
      if (msg.type !== 'pack_import') return
      const entry: PackProgressEntry = {
        stage: msg.stage,
        percent: msg.percent,
        state: msg.state ?? 'running',
        message: msg.message,
      }
      setProgress((prev) => ({ ...prev, [msg.pack_id]: entry }))
      if (msg.state === 'completed') {
        const isBuild = msg.stage === 'build'
        toast.success(
          isBuild
            ? t('toast.buildCompleted', { defaultValue: '整合包构建完成' })
            : t('toast.importCompleted', { defaultValue: '整合包导入完成' }),
          { description: msg.message ?? msg.pack_id },
        )
        void loadPacks()
      } else if (msg.state === 'failed') {
        toast.error(t('toast.failed', { defaultValue: '整合包操作失败' }), {
          description: msg.message ?? msg.pack_id,
        })
      }
    })
  }, [loadPacks, t])

  /** 受理导入/构建后登记初始进度条目 */
  function trackProgress(packId: string, stage: string) {
    setProgress((prev) => ({
      ...prev,
      [packId]: { state: 'running', stage, percent: 0 },
    }))
  }

  // ── 卸载 ──
  const [uninstalling, setUninstalling] = useState(false)
  async function confirmUninstall() {
    if (!uninstallTarget || uninstalling) return
    setUninstalling(true)
    try {
      await api.deletePack(uninstallTarget.id, keepModels)
      toast.success(t('toast.uninstalled', { defaultValue: '整合包已卸载' }), {
        description: uninstallTarget.id,
      })
      setUninstallTarget(null)
      await loadPacks()
    } catch (e) {
      toast.error(t('toast.uninstallFailed', { defaultValue: '卸载失败' }), {
        description: errMsg(e),
      })
    } finally {
      setUninstalling(false)
    }
  }

  const progressEntries = Object.entries(progress)

  return (
    <PageContainer
      title={t('page.title', { defaultValue: '整合包' })}
      description={t('page.description', {
        defaultValue: '导入、构建与管理模型整合包（.epzip）',
      })}
      actions={
        <>
          <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
            <Upload className="size-3.5" />
            {t('action.import', { defaultValue: '导入整合包' })}
          </Button>
          <Button size="sm" onClick={() => setBuildOpen(true)}>
            <PackagePlus className="size-3.5" />
            {t('action.build', { defaultValue: '构建整合包' })}
          </Button>
          <Button variant="outline" size="sm" onClick={() => void loadPacks()}>
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
            <span className="min-w-0 flex-1 truncate">{errMsg(error)}</span>
            <Button variant="ghost" size="xs" onClick={() => void loadPacks()}>
              {t('common:action.retry')}
            </Button>
          </div>
        )}

        {/* 导入 / 构建进度（WS pack_import 聚合） */}
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
                          ? t(`stage.${entry.stage}`, { defaultValue: entry.stage })
                          : '')}
                    </span>
                    {typeof entry.percent === 'number' && entry.state === 'running' ? (
                      <span className="font-mono text-muted-foreground">
                        {Math.floor(entry.percent)}%
                      </span>
                    ) : null}
                  </div>
                  {entry.state === 'running' ? (
                    <Progress
                      value={entry.percent ?? 0}
                      className={cn('h-1.5', entry.percent === undefined && 'animate-pulse')}
                    />
                  ) : null}
                </div>
                {entry.state !== 'running' ? (
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    onClick={() =>
                      setProgress((prev) => {
                        const next = { ...prev }
                        delete next[packId]
                        return next
                      })
                    }
                    aria-label={t('common:action.close')}
                  >
                    <X className="size-3.5" />
                  </Button>
                ) : null}
              </div>
            ))}
          </div>
        )}

        {loading ? (
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
            {Array.from({ length: 3 }).map((_, i) => (
              <Skeleton key={i} className="h-44 rounded-lg" />
            ))}
          </div>
        ) : !packs || packs.length === 0 ? (
          <Card>
            <EmptyState
              icon={PackageOpen}
              title={t('empty.title', { defaultValue: '暂无整合包' })}
              description={t('empty.description', {
                defaultValue: '导入或构建整合包后将在此显示',
              })}
              action={{
                label: t('action.import', { defaultValue: '导入整合包' }),
                onClick: () => setImportOpen(true),
              }}
            />
          </Card>
        ) : (
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
            {packs.map((pack) => (
              <Card key={pack.id} className="flex flex-col">
                <CardHeader className="pb-2">
                  <div className="flex items-start justify-between gap-2">
                    <CardTitle className="min-w-0 text-base font-semibold">
                      <span className="block truncate">{pack.name}</span>
                    </CardTitle>
                    <Badge
                      variant="secondary"
                      className="shrink-0 font-mono text-[10px] text-muted-foreground"
                    >
                      v{pack.version}
                    </Badge>
                  </div>
                  <CardDescription className="truncate font-mono text-xs">
                    {pack.id}
                  </CardDescription>
                </CardHeader>
                <CardContent className="flex flex-1 flex-col gap-3">
                  {pack.description ? (
                    <p className="line-clamp-2 text-xs text-muted-foreground">
                      {pack.description}
                    </p>
                  ) : null}
                  <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                    <span>
                      {t('card.modelsCount', {
                        defaultValue: '{{count}} 个模型',
                        count: pack.models?.length ?? 0,
                      })}
                    </span>
                    <span>
                      {t('card.pipelinesCount', {
                        defaultValue: '{{count}} 条管线',
                        count: pack.pipelines?.length ?? 0,
                      })}
                    </span>
                    {pack.installed_at ? (
                      <span className="font-mono text-[10px]">
                        {formatInstalledAt(pack.installed_at)}
                      </span>
                    ) : null}
                  </div>
                  {(pack.tags ?? []).length > 0 ? (
                    <div className="flex flex-wrap gap-1">
                      {(pack.tags ?? []).map((tag) => (
                        <Badge
                          key={tag}
                          variant="outline"
                          className="px-1.5 text-[10px] font-normal text-muted-foreground"
                        >
                          {tag}
                        </Badge>
                      ))}
                    </div>
                  ) : null}
                  <div className="mt-auto flex flex-wrap items-center gap-1.5 pt-1">
                    <Button size="xs" variant="outline" onClick={() => setDetailId(pack.id)}>
                      <Info className="size-3" />
                      {t('common:label.details')}
                    </Button>
                    <a href={api.packExportUrl(pack.id)} download>
                      <Button size="xs" variant="outline">
                        <Download className="size-3" />
                        {t('action.export', { defaultValue: '导出' })}
                      </Button>
                    </a>
                    <Button
                      size="xs"
                      variant="ghost"
                      className="text-muted-foreground hover:text-destructive"
                      onClick={() => {
                        setKeepModels(true)
                        setUninstallTarget(pack)
                      }}
                    >
                      <Trash2 className="size-3" />
                      {t('action.uninstall', { defaultValue: '卸载' })}
                    </Button>
                  </div>
                </CardContent>
              </Card>
            ))}
          </div>
        )}
      </div>

      {/* ── 导入对话框（本地路径 / URL / 浏览器上传）── */}
      <ImportDialog
        open={importOpen}
        onClose={() => setImportOpen(false)}
        onAccepted={(packId) => {
          trackProgress(packId, 'accepted')
          setImportOpen(false)
        }}
      />

      {/* ── 构建向导 ── */}
      <BuildWizardDialog
        open={buildOpen}
        onClose={() => setBuildOpen(false)}
        onAccepted={(packId) => {
          trackProgress(packId, 'build')
          setBuildOpen(false)
        }}
      />

      {/* ── 详情抽屉 ── */}
      <PackDetailSheet packId={detailId} onClose={() => setDetailId(null)} />

      {/* ── 卸载确认（keep_models 选项）── */}
      <Dialog
        open={uninstallTarget !== null}
        onOpenChange={(open) => {
          if (!open) setUninstallTarget(null)
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Trash2 className="size-4 text-destructive" />
              {t('uninstall.title', {
                defaultValue: '卸载整合包「{{id}}」？',
                id: uninstallTarget?.id ?? '',
              })}
            </DialogTitle>
            <DialogDescription>
              {t('uninstall.description', {
                defaultValue: '移除注册条目与其安装的管线；模型文件可选保留',
              })}
            </DialogDescription>
          </DialogHeader>
          <label className="flex cursor-pointer items-center gap-2.5 rounded-md border border-border px-3 py-2.5 text-sm">
            <Switch checked={keepModels} onCheckedChange={setKeepModels} />
            {t('uninstall.keepModels', {
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
              {t('action.uninstall', { defaultValue: '卸载' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  )
}

// ─── 导入对话框 ──────────────────────────────────────────────────────────────

function ImportDialog({
  open,
  onClose,
  onAccepted,
}: {
  open: boolean
  onClose: () => void
  onAccepted: (packId: string) => void
}) {
  const { t } = useTranslation('packs')
  const [localPath, setLocalPath] = useState('')
  const [url, setUrl] = useState('')
  const [pickedFile, setPickedFile] = useState<File | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const [uploadPercent, setUploadPercent] = useState<number | null>(null)
  const fileInputRef = useRef<HTMLInputElement | null>(null)
  const abortRef = useRef<(() => void) | null>(null)

  // 关闭时复位（上传中止）
  useEffect(() => {
    if (!open) {
      abortRef.current?.()
      abortRef.current = null
      setSubmitting(false)
      setUploadPercent(null)
      setPickedFile(null)
      setLocalPath('')
      setUrl('')
    }
  }, [open])

  async function submitLocal() {
    if (!localPath.trim() || submitting) return
    setSubmitting(true)
    try {
      const resp = await api.importPack({ source: 'local', path: localPath.trim() })
      toast.success(t('toast.accepted', { defaultValue: '整合包导入已受理' }), {
        description: resp.pack_id,
      })
      onAccepted(resp.pack_id)
    } catch (e) {
      toast.error(t('toast.importFailed', { defaultValue: '导入受理失败' }), {
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
      const resp = await api.importPack({ source: 'url', url: url.trim() })
      toast.success(t('toast.accepted', { defaultValue: '整合包导入已受理' }), {
        description: resp.pack_id,
      })
      onAccepted(resp.pack_id)
    } catch (e) {
      toast.error(t('toast.importFailed', { defaultValue: '导入受理失败' }), {
        description: errMsg(e),
      })
    } finally {
      setSubmitting(false)
    }
  }

  async function submitUpload() {
    if (!pickedFile || submitting) return
    setSubmitting(true)
    setUploadPercent(0)
    const { promise, abort } = uploadPackWithProgress(pickedFile, (percent) =>
      setUploadPercent(percent),
    )
    abortRef.current = abort
    try {
      const resp = await promise
      toast.success(t('toast.accepted', { defaultValue: '整合包导入已受理' }), {
        description: resp.pack_id,
      })
      onAccepted(resp.pack_id)
    } catch (e) {
      if (e instanceof Error && e.message === '__ep_pack_upload_aborted__') {
        toast.info(t('toast.uploadCancelled', { defaultValue: '上传已取消' }))
      } else {
        toast.error(t('toast.importFailed', { defaultValue: '导入受理失败' }), {
          description: errMsg(e),
        })
      }
    } finally {
      abortRef.current = null
      setSubmitting(false)
      setUploadPercent(null)
    }
  }

  return (
    <Dialog open={open} onOpenChange={(v) => (v ? undefined : onClose())}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Upload className="size-4 text-primary" />
            {t('import.title', { defaultValue: '导入整合包' })}
          </DialogTitle>
          <DialogDescription>
            {t('import.description', {
              defaultValue:
                '支持服务器本地路径、URL 下载与浏览器上传（.epzip）；受理后进度实时推送',
            })}
          </DialogDescription>
        </DialogHeader>

        <Tabs defaultValue="local">
          <TabsList>
            <TabsTrigger value="local">
              <HardDrive className="size-3.5" />
              {t('import.tabLocal', { defaultValue: '本地路径' })}
            </TabsTrigger>
            <TabsTrigger value="url">
              <Globe className="size-3.5" />
              URL
            </TabsTrigger>
            <TabsTrigger value="upload">
              <FileArchive className="size-3.5" />
              {t('import.tabUpload', { defaultValue: '上传' })}
            </TabsTrigger>
          </TabsList>

          <TabsContent value="local" className="mt-4 space-y-3">
            <Input
              value={localPath}
              onChange={(e) => setLocalPath(e.target.value)}
              placeholder={t('import.localPlaceholder', {
                defaultValue: '服务器上 .epzip 文件的绝对路径',
              })}
              className="font-mono text-xs"
              disabled={submitting}
            />
            <div className="flex justify-end">
              <Button disabled={!localPath.trim() || submitting} onClick={() => void submitLocal()}>
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
              {t('import.urlHint', {
                defaultValue: '后端先下载归档再受理，大文件等待时间较长',
              })}
            </p>
            <div className="flex justify-end">
              <Button disabled={!url.trim() || submitting} onClick={() => void submitUrl()}>
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
                {t('import.pickFile', { defaultValue: '选择 .epzip 文件' })}
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
            {uploadPercent !== null && (
              <div className="space-y-1">
                <div className="flex justify-between font-mono text-[11px] text-muted-foreground">
                  <span>{t('import.uploading', { defaultValue: '正在上传' })}</span>
                  <span>{Math.floor(uploadPercent)}%</span>
                </div>
                <Progress value={uploadPercent} className="h-1.5" />
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
                {submitting ? <Loader2 className="size-4 animate-spin" /> : <Upload className="size-4" />}
                {t('import.startUpload', { defaultValue: '上传并导入' })}
              </Button>
            </div>
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  )
}

// ─── 构建向导（tag 圈选 / 逐个勾选 + 管线选择 + 自定义包身份）────────────────

/** 构建候选模型：已就绪且带 qualified_id（后端圈选按 meta 匹配，仲裁 #22 限制） */
interface BuildCandidate {
  moduleId: string
  model: ModelInfo
  pin: string
}

function BuildWizardDialog({
  open,
  onClose,
  onAccepted,
}: {
  open: boolean
  onClose: () => void
  onAccepted: (packId: string) => void
}) {
  const { t } = useTranslation('packs')
  const [candidates, setCandidates] = useState<BuildCandidate[]>([])
  const [pipelines, setPipelines] = useState<PipelineSummary[]>([])
  const [loadingData, setLoadingData] = useState(false)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [bundled, setBundled] = useState<Set<string>>(new Set())
  const [selectedPipelines, setSelectedPipelines] = useState<Set<string>>(new Set())
  const [tagFilters, setTagFilters] = useState<Set<string>>(new Set())
  const [packId, setPackId] = useState('')
  const [packName, setPackName] = useState('')
  const [packVersion, setPackVersion] = useState('')
  const [packDescription, setPackDescription] = useState('')
  const [submitting, setSubmitting] = useState(false)

  // 打开时加载候选（已就绪模型 + 管线列表）并复位表单
  useEffect(() => {
    if (!open) return
    setSelected(new Set())
    setBundled(new Set())
    setSelectedPipelines(new Set())
    setTagFilters(new Set())
    setPackId('')
    setPackName('')
    setPackVersion('')
    setPackDescription('')
    setLoadingData(true)
    Promise.allSettled([api.models(), api.listPipelines()])
      .then(([modelsRes, pipelinesRes]) => {
        const list: BuildCandidate[] = []
        if (modelsRes.status === 'fulfilled') {
          for (const group of modelsRes.value.modules) {
            for (const model of group.models) {
              if (!isReady(model.status) || !model.qualified_id) continue
              list.push({
                moduleId: group.module_id,
                model,
                pin: `${model.qualified_id}@${model.model_id}`,
              })
            }
          }
        }
        setCandidates(list)
        setPipelines(pipelinesRes.status === 'fulfilled' ? pipelinesRes.value : [])
      })
      .finally(() => setLoadingData(false))
  }, [open])

  function isReady(status: string): boolean {
    return status.trim().toLowerCase() === 'ready'
  }

  /** 全部候选 tag（圈选快捷入口，§4.5 tag 组装闭环） */
  const allTags = useMemo(() => {
    const out: string[] = []
    for (const c of candidates) {
      for (const tag of c.model.tags ?? []) {
        if (!out.includes(tag)) out.push(tag)
      }
    }
    return out.sort((a, b) => a.localeCompare(b))
  }, [candidates])

  /** tag 圈选：勾选/取消该 tag 下全部候选模型 */
  function toggleTagSelect(tag: string) {
    setTagFilters((prev) => {
      const next = new Set(prev)
      const selecting = !next.has(tag)
      if (selecting) next.add(tag)
      else next.delete(tag)
      setSelected((sel) => {
        const nextSel = new Set(sel)
        for (const c of candidates) {
          if ((c.model.tags ?? []).includes(tag)) {
            if (selecting) nextSel.add(c.pin)
            else nextSel.delete(c.pin)
          }
        }
        return nextSel
      })
      return next
    })
  }

  function toggleModel(pin: string) {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(pin)) next.delete(pin)
      else next.add(pin)
      return next
    })
  }

  function toggleBundle(pin: string) {
    setBundled((prev) => {
      const next = new Set(prev)
      if (next.has(pin)) next.delete(pin)
      else next.add(pin)
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
      toast.error(t('build.invalidId', { defaultValue: '包 ID 格式无效' }), {
        description: t('build.invalidIdHint', {
          defaultValue: '需为 <publisher>.<pack-name>，仅小写字母/数字/连字符',
        }),
      })
      return
    }
    setSubmitting(true)
    try {
      // bundle 列表使用 qualified_id（无 @variant）
      const bundleIds = [...bundled]
        .filter((pin) => selected.has(pin))
        .map((pin) => pin.split('@')[0])
      const resp = await api.buildPack({
        models: [...selected],
        pipelines: [...selectedPipelines],
        bundle: [...new Set(bundleIds)],
        ...(id ? { id } : {}),
        ...(packName.trim() ? { name: packName.trim() } : {}),
        ...(packVersion.trim() ? { version: packVersion.trim() } : {}),
        ...(packDescription.trim() ? { description: packDescription.trim() } : {}),
      })
      toast.success(t('toast.buildAccepted', { defaultValue: '整合包构建已受理' }), {
        description: resp.pack_id,
      })
      onAccepted(resp.pack_id)
    } catch (e) {
      toast.error(t('toast.buildFailed', { defaultValue: '构建受理失败' }), {
        description: errMsg(e),
      })
    } finally {
      setSubmitting(false)
    }
  }

  const nothingSelected = selected.size === 0 && selectedPipelines.size === 0

  return (
    <Dialog open={open} onOpenChange={(v) => (v ? undefined : onClose())}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <PackagePlus className="size-4 text-primary" />
            {t('build.title', { defaultValue: '构建整合包' })}
          </DialogTitle>
          <DialogDescription>
            {t('build.description', {
              defaultValue:
                '按 tag 圈选或逐个勾选模型，可附带管线；构建完成后可导出 .epzip 分发',
            })}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-5">
          {/* 包身份（可选，缺省自动生成，B2 扩展字段） */}
          <div className="space-y-2 rounded-lg border border-border p-3">
            <p className="text-sm font-medium">
              {t('build.identity', { defaultValue: '包身份（可选）' })}
            </p>
            <div className="grid gap-2 sm:grid-cols-2">
              <Input
                value={packId}
                onChange={(e) => setPackId(e.target.value)}
                placeholder={t('build.idPlaceholder', {
                  defaultValue: 'publisher.pack-name（留空自动生成）',
                })}
                className="font-mono text-xs"
              />
              <Input
                value={packName}
                onChange={(e) => setPackName(e.target.value)}
                placeholder={t('build.namePlaceholder', { defaultValue: '显示名称（可选）' })}
                className="text-xs"
              />
              <Input
                value={packVersion}
                onChange={(e) => setPackVersion(e.target.value)}
                placeholder={t('build.versionPlaceholder', {
                  defaultValue: '版本号，如 1.0.0（可选）',
                })}
                className="font-mono text-xs"
              />
              <Input
                value={packDescription}
                onChange={(e) => setPackDescription(e.target.value)}
                placeholder={t('build.descriptionPlaceholder', { defaultValue: '描述（可选）' })}
                className="text-xs"
              />
            </div>
          </div>

          {/* tag 圈选 */}
          {allTags.length > 0 && (
            <div className="space-y-2">
              <p className="text-sm font-medium">
                {t('build.tagSelect', { defaultValue: '按标签圈选' })}
              </p>
              <div className="flex flex-wrap gap-1.5">
                {allTags.map((tag) => {
                  const active = tagFilters.has(tag)
                  return (
                    <button
                      key={tag}
                      type="button"
                      onClick={() => toggleTagSelect(tag)}
                      className={cn(
                        'rounded-full border px-2.5 py-0.5 text-xs transition-colors',
                        active
                          ? 'border-primary bg-primary text-primary-foreground'
                          : 'border-border bg-muted/30 text-muted-foreground hover:bg-muted',
                      )}
                    >
                      {tag}
                    </button>
                  )
                })}
              </div>
            </div>
          )}

          {/* 模型逐个勾选 */}
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <p className="text-sm font-medium">
                {t('build.models', { defaultValue: '选择模型' })}
                <span className="ml-2 font-mono text-xs text-muted-foreground">
                  {selected.size}/{candidates.length}
                </span>
              </p>
            </div>
            {loadingData ? (
              <div className="space-y-2">
                <Skeleton className="h-9 rounded-md" />
                <Skeleton className="h-9 rounded-md" />
              </div>
            ) : candidates.length === 0 ? (
              <p className="rounded-md border border-dashed border-border px-3 py-4 text-center text-xs text-muted-foreground">
                {t('build.noCandidates', {
                  defaultValue:
                    '无可选模型：仅已就绪且带全限定 ID 的模型可入包（整合包导入的模型或已设置标签后下载的模型）',
                })}
              </p>
            ) : (
              <div className="max-h-56 divide-y divide-border overflow-y-auto rounded-lg border border-border">
                {candidates.map((c) => {
                  const checked = selected.has(c.pin)
                  return (
                    <div key={c.pin} className="flex items-center gap-2.5 px-3 py-2">
                      <input
                        type="checkbox"
                        className="size-4 shrink-0 accent-primary"
                        checked={checked}
                        onChange={() => toggleModel(c.pin)}
                        aria-label={c.pin}
                      />
                      <div className="min-w-0 flex-1">
                        <p className="truncate font-mono text-xs">{c.pin}</p>
                        <p className="flex flex-wrap items-center gap-1 text-[10px] text-muted-foreground">
                          {c.moduleId}
                          {(c.model.tags ?? []).map((tag) => (
                            <Badge
                              key={tag}
                              variant="outline"
                              className="px-1 text-[9px] font-normal"
                            >
                              {tag}
                            </Badge>
                          ))}
                        </p>
                      </div>
                      <label
                        className="flex shrink-0 cursor-pointer items-center gap-1.5 text-[11px] text-muted-foreground"
                        title={t('build.bundleHint', {
                          defaultValue: 'bundle：权重随包（体积大）；否则仅引用，导入时下载',
                        })}
                      >
                        <Switch
                          size="sm"
                          checked={bundled.has(c.pin)}
                          onCheckedChange={() => toggleBundle(c.pin)}
                          disabled={!checked}
                        />
                        {t('build.bundle', { defaultValue: '随包权重' })}
                      </label>
                    </div>
                  )
                })}
              </div>
            )}
          </div>

          {/* 管线选择 */}
          <div className="space-y-2">
            <p className="text-sm font-medium">
              {t('build.pipelines', { defaultValue: '附带管线（可选）' })}
            </p>
            {pipelines.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {t('build.noPipelines', { defaultValue: '暂无已注册管线' })}
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
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={submitting}>
            {t('common:action.cancel')}
          </Button>
          <Button disabled={nothingSelected || submitting} onClick={() => void handleBuild()}>
            {submitting ? <Loader2 className="size-4 animate-spin" /> : <PackagePlus className="size-4" />}
            {t('build.submit', { defaultValue: '开始构建' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ─── 详情抽屉（内容清单 + 适配报告）─────────────────────────────────────────

function PackDetailSheet({
  packId,
  onClose,
}: {
  packId: string | null
  onClose: () => void
}) {
  const { t } = useTranslation('packs')
  const [detail, setDetail] = useState<PackDetail | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)

  useEffect(() => {
    if (!packId) return
    setDetail(null)
    setLoadError(null)
    api
      .getPack(packId)
      .then(setDetail)
      .catch((e: unknown) => setLoadError(e instanceof Error ? e.message : String(e)))
  }, [packId])

  return (
    <Sheet
      open={packId !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <SheetContent className="overflow-y-auto sm:max-w-lg">
        <SheetHeader>
          <SheetTitle className="flex items-center gap-2">
            <PackageOpen className="size-4 text-primary" />
            {detail?.name ?? packId ?? ''}
          </SheetTitle>
          <SheetDescription className="font-mono text-xs">
            {detail ? `${detail.id} · v${detail.version}` : ''}
          </SheetDescription>
        </SheetHeader>

        <div className="space-y-5 px-4 pb-6">
          {loadError ? (
            <p className="text-sm text-status-error">{errMsg(loadError)}</p>
          ) : !detail ? (
            <div className="space-y-2">
              <Skeleton className="h-4 w-2/3" />
              <Skeleton className="h-4 w-1/2" />
              <Skeleton className="h-24 rounded-lg" />
            </div>
          ) : (
            <>
              {detail.description ? (
                <p className="text-sm text-foreground/80">{detail.description}</p>
              ) : null}
              {detail.installed_at ? (
                <p className="text-xs text-muted-foreground">
                  {t('detail.installedAt', { defaultValue: '安装时间' })}：
                  <span className="font-mono">{formatInstalledAt(detail.installed_at)}</span>
                </p>
              ) : null}

              {/* 内容清单 */}
              <div className="space-y-2">
                <h3 className="text-sm font-semibold">
                  {t('detail.contents', { defaultValue: '内容清单' })}
                </h3>
                {(detail.models ?? []).length === 0 ? (
                  <p className="text-xs text-muted-foreground">
                    {t('detail.noModels', { defaultValue: '包内无模型声明' })}
                  </p>
                ) : (
                  <div className="divide-y divide-border rounded-lg border border-border">
                    {(detail.models ?? []).map((m) => (
                      <div
                        key={`${m.qualified_id}@${m.variant ?? ''}`}
                        className="flex flex-wrap items-center gap-2 px-3 py-2"
                      >
                        <span className="min-w-0 flex-1 break-all font-mono text-xs">
                          {m.qualified_id}
                          {m.variant ? `@${m.variant}` : ''}
                        </span>
                        <Badge
                          variant={m.mode === 'bundle' ? 'default' : 'outline'}
                          className="px-1.5 text-[10px] font-normal"
                          title={t('detail.modeHint', {
                            defaultValue: 'bundle=权重随包；reference=导入时下载',
                          })}
                        >
                          {m.mode}
                        </Badge>
                        {(m.tags ?? []).map((tag) => (
                          <Badge
                            key={tag}
                            variant="secondary"
                            className="px-1.5 text-[10px] font-normal"
                          >
                            {tag}
                          </Badge>
                        ))}
                      </div>
                    ))}
                  </div>
                )}
                {(detail.pipelines ?? []).length > 0 ? (
                  <div className="flex flex-wrap gap-1.5">
                    {(detail.pipelines ?? []).map((p) => (
                      <Badge
                        key={p}
                        variant="outline"
                        className="font-mono text-[10px] font-normal text-muted-foreground"
                      >
                        {p}
                      </Badge>
                    ))}
                  </div>
                ) : null}
              </div>

              {/* 适配报告（§4.6） */}
              <div className="space-y-2">
                <h3 className="text-sm font-semibold">
                  {t('detail.adaptation', { defaultValue: '平台适配报告' })}
                </h3>
                {(detail.adaptation ?? []).length === 0 ? (
                  <p className="text-xs text-muted-foreground">
                    {t('detail.noAdaptation', { defaultValue: '无适配结论' })}
                  </p>
                ) : (
                  <div className="divide-y divide-border rounded-lg border border-border">
                    {(detail.adaptation ?? []).map((entry) => (
                      <div
                        key={`${entry.qualified_id}@${entry.variant ?? ''}`}
                        className="flex items-start gap-2.5 px-3 py-2"
                      >
                        {entry.ok ? (
                          <CircleCheck className="mt-0.5 size-3.5 shrink-0 text-status-running" />
                        ) : (
                          <CircleX className="mt-0.5 size-3.5 shrink-0 text-status-error" />
                        )}
                        <div className="min-w-0 flex-1">
                          <p className="break-all font-mono text-xs">
                            {entry.qualified_id}
                            {entry.variant ? `@${entry.variant}` : ''}
                          </p>
                          <p
                            className={cn(
                              'mt-0.5 text-xs',
                              entry.ok ? 'text-muted-foreground' : 'text-status-error',
                            )}
                          >
                            {entry.note ?? ''}
                            {entry.device ? (
                              <span className="ml-1.5 font-mono text-[10px]">
                                ({entry.device})
                              </span>
                            ) : null}
                          </p>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>

              {/* 导出下载 */}
              <a href={api.packExportUrl(detail.id)} download>
                <Button variant="outline" size="sm" className="w-full">
                  <Download className="size-3.5" />
                  {t('action.export', { defaultValue: '导出' })} .epzip
                </Button>
              </a>
            </>
          )}
        </div>
      </SheetContent>
    </Sheet>
  )
}
