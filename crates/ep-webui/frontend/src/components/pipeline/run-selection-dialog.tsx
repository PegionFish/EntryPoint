import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router-dom'
import { CircleCheck, Loader2, Play, TriangleAlert } from 'lucide-react'
import { toast } from 'sonner'

import type { BatchSubmitResponse } from '@/api/types'
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
import { ScrollArea } from '@/components/ui/scroll-area'
import { Switch } from '@/components/ui/switch'
import { cn } from '@/lib/utils'

/**
 * T-28c「作为组运行」对话框：把多选节点各自的直跑能力批量提交为任务组。
 *
 * 提交形状（api/batches.rs BatchRequest）：一次 POST /api/v1/batches =
 * 单一 module+capability × 服务器侧文件夹全部文件 → 一个 group_id，
 * 任务中心按组折叠展示（已有能力）。故每个选中节点各提交一个 batch
 * （各得一个组）；folder/递归/扩展名统一套用，params 取各节点当前参数。
 */
export interface RunGroupTarget {
  nodeId: string
  label: string
  moduleId: string
  capability: string
  /** 该节点当前参数（原样下发，由后端按 capability schema 校验） */
  params: Record<string, unknown>
}

/** 单个目标提交结果（逐条展示） */
export interface RunGroupOutcome {
  nodeId: string
  label: string
  ok: boolean
  /** 成功：组 id（任务中心按此折叠） */
  groupId?: string
  /** 成功：受理的成员任务数 */
  submitted?: number
  /** 失败：错误信息 */
  error?: string
}

export function RunSelectionDialog({
  open,
  onOpenChange,
  targets,
  onConfirm,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  targets: RunGroupTarget[]
  /** 提交单个目标（pipeline 层包装 api.submitBatch）；抛错 = 该目标失败 */
  onConfirm: (target: RunGroupTarget, opts: RunGroupOptions) => Promise<BatchSubmitResponse>
}) {
  const { t } = useTranslation('pipeline')
  const [folder, setFolder] = useState('workspace/uploads')
  const [recursive, setRecursive] = useState(false)
  const [extensionsText, setExtensionsText] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [progress, setProgress] = useState(0)
  const [outcomes, setOutcomes] = useState<RunGroupOutcome[] | null>(null)

  useEffect(() => {
    if (open) {
      setFolder('workspace/uploads')
      setRecursive(false)
      setExtensionsText('')
      setSubmitting(false)
      setProgress(0)
      setOutcomes(null)
    }
  }, [open])

  const folderOk = folder.trim().length > 0
  const extensions = extensionsText
    .split(/[,，\s]+/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
    .map((s) => (s.startsWith('.') ? s : `.${s}`))

  const submitted = outcomes?.filter((o) => o.ok) ?? []
  const failed = outcomes?.filter((o) => !o.ok) ?? []

  const handleSubmit = async () => {
    if (!folderOk || submitting || targets.length === 0) return
    setSubmitting(true)
    setProgress(0)
    const collected: RunGroupOutcome[] = []
    for (const target of targets) {
      try {
        const resp = await onConfirm(target, {
          folder: folder.trim(),
          recursive,
          extensions,
        })
        collected.push({
          nodeId: target.nodeId,
          label: target.label,
          ok: true,
          groupId: resp.group_id,
          submitted: resp.submitted,
        })
      } catch (err) {
        collected.push({
          nodeId: target.nodeId,
          label: target.label,
          ok: false,
          error: err instanceof Error ? err.message : String(err),
        })
      }
      setProgress(collected.length)
      setOutcomes([...collected])
    }
    setSubmitting(false)
    const okCount = collected.filter((o) => o.ok).length
    if (okCount > 0) {
      toast.success(
        t('runGroup.submittedToast', {
          defaultValue: '已提交 {{count}} 个任务组',
          count: okCount,
        }),
        {
          description: t('runGroup.submittedHint', {
            defaultValue: '任务中心将按组折叠展示；成员任务仍持独立 task_id。',
          }),
        },
      )
    }
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !submitting && onOpenChange(next)}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {t('runGroup.dialogTitle', { defaultValue: '作为组运行（{{count}} 个节点）', count: targets.length })}
          </DialogTitle>
          <DialogDescription>
            {t('runGroup.dialogDescription', {
              defaultValue:
                '对服务器侧文件夹批量执行各选中节点的直跑能力：每个节点一个任务组，成员任务逐文件独立受理。',
            })}
          </DialogDescription>
        </DialogHeader>

        {outcomes === null ? (
          <div className="space-y-4 py-1">
            <div className="space-y-1.5">
              <span className="text-xs font-medium">
                {t('runGroup.folder', { defaultValue: '服务器侧文件夹' })}
                <span className="ml-0.5 text-status-error" aria-label={t('common:label.required')}>
                  *
                </span>
              </span>
              <Input
                value={folder}
                onChange={(e) => setFolder(e.target.value)}
                placeholder="workspace/uploads"
                className="h-8 font-mono text-xs"
              />
              <p className="text-[10px] text-muted-foreground">
                {t('runGroup.folderHint', {
                  defaultValue:
                    '缺省仅接受 workspace/uploads 内的目录（越权返回 403 FOLDER_NOT_ALLOWED）。',
                })}
              </p>
            </div>

            <div className="flex items-center justify-between rounded-lg border border-border px-3 py-2.5">
              <div className="min-w-0">
                <p className="text-xs font-medium">
                  {t('runGroup.recursive', { defaultValue: '递归子目录' })}
                </p>
                <p className="text-[10px] text-muted-foreground">
                  {t('runGroup.recursiveHint', { defaultValue: '关闭时仅枚举第一层文件' })}
                </p>
              </div>
              <Switch checked={recursive} onCheckedChange={setRecursive} />
            </div>

            <div className="space-y-1.5">
              <span className="text-xs font-medium">
                {t('runGroup.extensions', { defaultValue: '扩展名过滤（可选）' })}
              </span>
              <Input
                value={extensionsText}
                onChange={(e) => setExtensionsText(e.target.value)}
                placeholder=".png, .jpg"
                className="h-8 font-mono text-xs"
              />
              <p className="text-[10px] text-muted-foreground">
                {t('runGroup.extensionsHint', { defaultValue: '逗号分隔，大小写不敏感；留空不过滤' })}
              </p>
            </div>

            <div className="space-y-1">
              <span className="text-xs font-medium">
                {t('runGroup.targetsTitle', { defaultValue: '将提交的任务组' })}
              </span>
              <ScrollArea className="max-h-40">
                <div className="space-y-1 p-1 pr-3">
                  {targets.map((target) => (
                    <div
                      key={target.nodeId}
                      className="flex items-baseline gap-2 rounded-md border border-border px-2.5 py-1.5 text-[11px]"
                    >
                      <span className="min-w-0 flex-1 truncate">{target.label}</span>
                      <span className="shrink-0 font-mono text-muted-foreground">
                        {target.moduleId}/{target.capability}
                      </span>
                      <span
                        className="shrink-0 rounded bg-muted px-1 text-[9px] text-muted-foreground"
                        title={t('runGroup.paramCount', {
                          defaultValue: '{{count}} 个参数随组下发',
                          count: Object.keys(target.params).length,
                        })}
                      >
                        {Object.keys(target.params).length}
                      </span>
                    </div>
                  ))}
                </div>
              </ScrollArea>
            </div>
          </div>
        ) : (
          <div className="space-y-1.5 py-1">
            <p className="text-xs text-muted-foreground">
              {submitting
                ? t('runGroup.progress', {
                    defaultValue: '提交中… {{done}}/{{count}}',
                    done: progress,
                    count: targets.length,
                  })
                : t('runGroup.doneSummary', {
                    defaultValue: '完成：{{ok}} 个组受理成功，{{failed}} 个失败',
                    ok: submitted.length,
                    failed: failed.length,
                  })}
            </p>
            {outcomes.map((outcome) => (
              <div
                key={outcome.nodeId}
                className={cn(
                  'flex items-baseline gap-2 rounded-md border px-2.5 py-1.5 text-[11px]',
                  outcome.ok
                    ? 'border-status-running/40 bg-status-running/5'
                    : 'border-status-error/40 bg-status-error/5',
                )}
              >
                {outcome.ok ? (
                  <CircleCheck className="h-3 w-3 shrink-0 self-center text-status-running" />
                ) : (
                  <TriangleAlert className="h-3 w-3 shrink-0 self-center text-status-error" />
                )}
                <span className="min-w-0 flex-1 truncate">{outcome.label}</span>
                {outcome.ok ? (
                  <span
                    className="shrink-0 font-mono text-muted-foreground"
                    title={t('runGroup.groupIdTitle', { defaultValue: '任务组 id' })}
                  >
                    {outcome.groupId} · {outcome.submitted}
                  </span>
                ) : (
                  <span className="max-w-56 shrink-0 truncate font-mono text-status-error" title={outcome.error}>
                    {outcome.error}
                  </span>
                )}
              </div>
            ))}
            {submitted.length > 0 && (
              <Link
                to="/tasks"
                className="inline-block pt-1 text-xs text-primary underline-offset-4 hover:underline"
              >
                {t('runGroup.goTasks', { defaultValue: '前往任务中心查看（按组折叠）' })}
              </Link>
            )}
          </div>
        )}

        <DialogFooter className="gap-2">
          {outcomes !== null && !submitting ? (
            <Button variant="outline" size="sm" onClick={() => onOpenChange(false)}>
              {t('common:action.close')}
            </Button>
          ) : (
            <>
              <Button
                variant="outline"
                size="sm"
                onClick={() => onOpenChange(false)}
                disabled={submitting}
              >
                {t('common:action.cancel')}
              </Button>
              <Button size="sm" onClick={() => void handleSubmit()} disabled={!folderOk || submitting}>
                {submitting ? <Loader2 className="animate-spin" /> : <Play />}
                {t('runGroup.submit', { defaultValue: '提交任务组' })}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** 提交选项（表单收集后传给 onConfirm） */
export interface RunGroupOptions {
  folder: string
  recursive: boolean
  extensions: string[]
}
