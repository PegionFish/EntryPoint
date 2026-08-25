import { useEffect, useMemo, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { ArrowRight, Loader2, Play, TriangleAlert, Upload } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { api } from '@/api/client'
import type { CapabilityParamSchema } from '@/api/types'
import { postExecuteSingle } from '@/hooks/use-direct-exec'
import type { CatalogEntry } from '@/components/quick-run/capability-catalog'
import { ParamField } from '@/components/quick-run/param-field'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/** text/json 输入型能力 → textarea；其余（audio/video/image/file）→ 文件 */
function isTextInput(capabilityInputType: string): boolean {
  const t = capabilityInputType.trim().toLowerCase()
  return t === 'text' || t === 'json'
}

/** 按 schema 预填参数默认值（与直跑抽屉同口径） */
function defaultParamsOf(
  params: Record<string, CapabilityParamSchema> | null | undefined,
): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  if (!params) return out
  for (const [name, schema] of Object.entries(params)) {
    if (schema.default !== undefined && schema.default !== null) {
      out[name] = schema.default
    }
  }
  return out
}

/**
 * 快速调用 · 右侧工作台（QUICK_RUN_PLAN D1/D2/D3）：
 * 能力详情 → 输入区（文件 = 路径直填/上传；text/json = textarea）→
 * 参数表单（schema 数据驱动）→ 提交（lazy_start=true 受理即返回）。
 * 任务进度在页内会话任务区呈现，本组件只负责提交。
 */
export function RunWorkbench({
  entry,
  idleMinutes,
  onSubmitted,
}: {
  entry: CatalogEntry
  /** 模块空闲自动下线阈值（分钟）；0/null = 常驻 */
  idleMinutes: number | null
  onSubmitted: (taskId: string) => void
}) {
  const { t } = useTranslation('run')
  const { t: tModels } = useTranslation('models')
  const cap = entry.capability

  const [params, setParams] = useState<Record<string, unknown>>({})
  const [inputPath, setInputPath] = useState('')
  const [inputText, setText] = useState('')
  const [uploading, setUploading] = useState(false)
  const [submitting, setSubmitting] = useState(false)
  const fileInputRef = useRef<HTMLInputElement | null>(null)

  // 切换能力条目：重置输入，按 schema 预填默认参数
  useEffect(() => {
    setInputPath('')
    setText('')
    setParams(defaultParamsOf(cap.params))
  }, [entry.key, cap.params])

  const paramEntries = useMemo(
    () =>
      Object.entries(cap.params ?? {}).sort(([a], [b]) =>
        a.localeCompare(b),
      ),
    [cap],
  )

  async function handleUpload(file: File) {
    setUploading(true)
    try {
      const resp = await api.uploadInput(file)
      setInputPath(resp.path)
      toast.success(tModels('run.uploadDone'), { description: resp.path })
    } catch (e) {
      toast.error(tModels('run.uploadFailed'), { description: errMsg(e) })
    } finally {
      setUploading(false)
    }
  }

  async function handleSubmit() {
    if (submitting || uploading) return
    const textMode = isTextInput(cap.input_type)
    if (textMode) {
      if (!inputText.trim()) return
      // json 输入型：提交前本地校验合法性，避免整次任务白跑
      if (cap.input_type.trim().toLowerCase() === 'json') {
        try {
          JSON.parse(inputText)
        } catch (err: unknown) {
          toast.error(t('previewFail'), { description: errMsg(err) })
          return
        }
      }
    } else if (!inputPath.trim()) {
      return
    }

    setSubmitting(true)
    try {
      const resp = await postExecuteSingle({
        module_id: entry.moduleId,
        capability: cap.name,
        params,
        lazy_start: true,
        ...(textMode ? { input_text: inputText } : { input_path: inputPath.trim() }),
      })
      toast.success(t('accepted'), { description: resp.task_id })
      onSubmitted(resp.task_id)
    } catch (e) {
      if (
        e instanceof Error &&
        e.message === '__ep_direct_exec_submit_timeout__'
      ) {
        toast.error(tModels('run.submitTimeout'), {
          description: tModels('run.submitTimeoutDesc'),
        })
      } else {
        toast.error(tModels('run.submitFailed'), { description: errMsg(e) })
      }
    } finally {
      setSubmitting(false)
    }
  }

  const textMode = isTextInput(cap.input_type)
  const canSubmit =
    !submitting &&
    !uploading &&
    entry.modelReady !== false &&
    (textMode ? inputText.trim().length > 0 : inputPath.trim().length > 0)

  return (
    <div className="space-y-5 rounded-xl border border-border bg-card p-4">
      {/* 0. 模型未就绪引导 */}
      {entry.modelReady === false && (
        <div className="flex flex-wrap items-center gap-2 rounded-lg border border-status-error/30 bg-status-error/10 px-3 py-2 text-sm">
          <TriangleAlert className="size-4 shrink-0 text-status-error" />
          <span>{t('modelNotReady')}</span>
          <Link
            to="/modules"
            className="ml-auto inline-flex items-center gap-1 text-primary hover:underline"
          >
            {t('goModules')}
            <ArrowRight className="size-3.5" />
          </Link>
        </div>
      )}

      {/* 1. 能力详情 */}
      <div className="space-y-1">
        <div className="flex items-baseline gap-2">
          <h2 className="font-mono text-sm font-semibold">{cap.name}</h2>
          <span className="truncate text-xs text-muted-foreground">
            {entry.moduleName}
          </span>
        </div>
        <p className="text-xs text-muted-foreground">
          {cap.description || '—'}
        </p>
        <p className="font-mono text-[10px] text-muted-foreground/80">
          {cap.input_type} → {cap.output_type}
          {cap.max_file_size_mb ? ` · ≤ ${cap.max_file_size_mb} MB` : ''}
        </p>
      </div>

      {/* 2. 输入区：text/json 走 textarea，其余走 文件路径/上传 */}
      <div className="space-y-2">
        <label className="text-sm font-medium">
          {textMode ? t('input.text') : t('input.file')}
        </label>
        {textMode ? (
          <textarea
            value={inputText}
            onChange={(e) => setText(e.target.value)}
            placeholder={t('input.textPlaceholder')}
            rows={6}
            className="min-h-24 w-full rounded-md border border-input bg-transparent px-3 py-2 font-mono text-xs shadow-xs transition-[color,box-shadow,border-color] duration-150 outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:opacity-50 dark:bg-input/30 focus-visible:border-ring focus-visible:shadow-[0_0_0_3px_var(--ring-glow)]"
            disabled={submitting}
          />
        ) : (
          <div className="flex gap-2">
            <Input
              value={inputPath}
              onChange={(e) => setInputPath(e.target.value)}
              placeholder={t('input.pathPlaceholder')}
              className="font-mono text-xs"
              disabled={submitting}
            />
            <Button
              variant="outline"
              size="sm"
              className="shrink-0"
              disabled={uploading || submitting}
              onClick={() => fileInputRef.current?.click()}
            >
              {uploading ? (
                <Loader2 className="size-3.5 animate-spin" />
              ) : (
                <Upload className="size-3.5" />
              )}
              {uploading ? t('input.uploading') : t('input.upload')}
            </Button>
            <input
              ref={fileInputRef}
              type="file"
              className="hidden"
              onChange={(e) => {
                const file = e.target.files?.[0]
                if (file) void handleUpload(file)
                e.target.value = ''
              }}
            />
          </div>
        )}
        {!textMode && (
          <p className="text-[11px] text-muted-foreground">{t('input.hint')}</p>
        )}
      </div>

      {/* 3. 参数表单（schema 数据驱动，复用抽取组件） */}
      {paramEntries.length > 0 && (
        <div className="space-y-3">
          <label className="text-sm font-medium">{t('params')}</label>
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

      {/* 4. 执行 */}
      <div className="space-y-2">
        <Button className="w-full" disabled={!canSubmit} onClick={() => void handleSubmit()}>
          {submitting ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <Play className="size-4" />
          )}
          {t('submit')}
        </Button>
        <p className="text-[11px] text-muted-foreground">
          {t('startingHint')}
        </p>
        {idleMinutes !== null && idleMinutes > 0 && (
          <p className="text-[11px] text-muted-foreground/80">
            {t('idleHint', { minutes: idleMinutes })}
          </p>
        )}
      </div>
    </div>
  )
}
