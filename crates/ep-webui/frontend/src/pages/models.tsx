import { useState } from 'react'
import {
  ChevronRight,
  Database,
  Download,
  FileBox,
  FolderOpen,
  HardDrive,
  Loader2,
  RefreshCw,
  TriangleAlert,
} from 'lucide-react'
import { toast } from 'sonner'
import type { ModelInfo } from '@/api/types'
import { PageContainer } from '@/components/layout/page-container'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { useModels } from '@/hooks/use-models'
import { cn, formatBytes, formatMB } from '@/lib/utils'

interface ModelStatusMeta {
  label: string
  dot: string
  badge: string
  transitional: boolean
}

/** 模型状态 → 元信息（ready=绿 / missing=红 / incomplete=黄） */
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

export function ModelsPage() {
  const { models, details, moduleModels, importModel, refresh, loading, error } =
    useModels()

  /** 已展开的模型行（key = module_id/model_id） */
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [detailLoading, setDetailLoading] = useState<Record<string, boolean>>(
    {},
  )

  // 导入表单状态
  const [importModule, setImportModule] = useState<string>('')
  const [importModelId, setImportModelId] = useState<string>('')
  const [sourcePath, setSourcePath] = useState('')
  const [importing, setImporting] = useState(false)

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

  const importModuleOptions = models?.modules ?? []
  const importModelOptions =
    importModuleOptions.find((m) => m.module_id === importModule)?.models ?? []

  return (
    <PageContainer
      title="模型管理"
      description="按模块查看模型状态，导入本地模型文件"
      actions={
        <Button variant="outline" size="sm" onClick={() => void refresh()}>
          <RefreshCw className="size-3.5" />
          刷新
        </Button>
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

        {loading ? (
          <div className="space-y-4">
            {Array.from({ length: 2 }).map((_, i) => (
              <Skeleton key={i} className="h-40 rounded-lg" />
            ))}
          </div>
        ) : !models || models.modules.length === 0 ? (
          <div className="flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border py-16 text-center">
            <Database className="size-8 text-muted-foreground/50" />
            <p className="text-sm text-muted-foreground">暂无模块模型信息</p>
            <p className="text-xs text-muted-foreground/70">
              安装模块后，其所需模型将在此处显示
            </p>
          </div>
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
                      return (
                        <div key={model.model_id}>
                          <button
                            type="button"
                            onClick={() =>
                              void toggleExpand(group.module_id, model)
                            }
                            className="flex w-full items-center gap-3 px-6 py-3.5 text-left transition-colors hover:bg-muted/50"
                          >
                            <ChevronRight
                              className={cn(
                                'size-4 shrink-0 text-muted-foreground transition-transform',
                                isOpen && 'rotate-90',
                              )}
                            />
                            <div className="min-w-0 flex-1">
                              <div className="truncate text-sm font-medium">
                                {model.name}
                              </div>
                              <div className="truncate font-mono text-xs text-muted-foreground">
                                {model.model_id}
                                {model.source && (
                                  <span className="ml-2 opacity-70">
                                    {model.source}
                                  </span>
                                )}
                              </div>
                            </div>
                            <ModelStatusBadge status={model.status} />
                            <span className="w-20 shrink-0 text-right font-mono text-xs text-muted-foreground">
                              {formatMB(model.size_estimate_mb)}
                            </span>
                          </button>
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

        {/* ── 导入模型 ── */}
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base font-semibold">
              <Download className="size-4 text-primary" />
              导入模型
            </CardTitle>
            <CardDescription>
              将本地模型文件复制到模块缓存目录，跳过网络下载
            </CardDescription>
          </CardHeader>
          <CardContent>
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
                    {importModuleOptions.map((m) => (
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
          </CardContent>
        </Card>
      </div>
    </PageContainer>
  )
}
