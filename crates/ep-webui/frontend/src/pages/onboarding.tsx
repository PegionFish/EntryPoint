import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { normalizeLanguage, setAppLanguage, type AppLanguage } from '@/i18n'
import { putConfigPatch } from '@/hooks/use-config'
import type { SetupStatus } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { cn } from '@/lib/utils'

/**
 * 首次运行向导（T-40，计划2 M1）。
 *
 * 门控在 App 级：仅当 GET /api/setup/status 报告 `initialized = false`
 * （config/app.toml 不存在）时全屏接管渲染；「完成」与「跳过」都经
 * PUT /api/config 落盘（跳过 = 空 patch，按当前值——全新环境即全缺省——
 * 落盘，行为与旧版一致，零强制）。落盘成功后探测即转为已初始化，
 * 后续启动 / 换浏览器不再弹出。
 *
 * 只做引导不承载业务逻辑（ep-desktop 红线延伸）：所有写入复用既有
 * PUT /api/config 合并语义，本组件零新增端点、零新增依赖。
 */

interface OnboardingPageProps {
  /** 首访探测结果（App 级门控时已取得），提供各步预填值 */
  status: SetupStatus
  /** 完成或跳过成功后回调（App 切回主界面） */
  onDone: () => void
}

const STEP_KEYS = [
  'step.welcome',
  'step.language',
  'step.workspace',
  'step.port',
  'step.done',
] as const

/** 语言步可选语言（与 i18n SUPPORTED_LANGUAGES 对齐，展示名固定不翻译） */
const LANGUAGE_OPTIONS: { value: AppLanguage; label: string }[] = [
  { value: 'zh-CN', label: '简体中文' },
  { value: 'en', label: 'English' },
]

/** 展示用相对路径解析预览（仅回显；真实解析以 daemon resolve 为准） */
function previewResolve(root: string, raw: string): string {
  const isAbsolute = /^[A-Za-z]:[\\/]/.test(raw) || /^[\\/]{2}/.test(raw) || raw.startsWith('/')
  if (isAbsolute) return raw
  const sep = root.includes('\\') ? '\\' : '/'
  const base = root.endsWith('\\') || root.endsWith('/') ? root : root + sep
  return base + raw
}

export function OnboardingPage({ status, onDone }: OnboardingPageProps) {
  const { t } = useTranslation('onboarding')
  // 0 欢迎 → 1 语言 → 2 工作目录 → 3 端口 → 4 完成
  const [step, setStep] = useState(0)
  const [language, setLanguage] = useState<AppLanguage>(
    normalizeLanguage(status.language),
  )
  const [workspaceDir, setWorkspaceDir] = useState(status.workspace_dir_raw)
  const [port, setPort] = useState(String(status.port))
  const [submitting, setSubmitting] = useState(false)

  const portNum = Number(port)
  const portValid = Number.isInteger(portNum) && portNum >= 1 && portNum <= 65535
  const workspaceValid = workspaceDir.trim().length > 0
  const workspaceTouched = workspaceDir !== status.workspace_dir_raw

  // 语言步选中即应用（视觉即时切换）；持久化统一在完成步一次写入
  const selectLanguage = (lang: AppLanguage) => {
    setLanguage(lang)
    setAppLanguage(lang)
  }

  /** 跳过 = 空 patch 落盘（当前值即缺省值，行为与旧版一致）→ 退出向导 */
  const skip = async () => {
    if (submitting) return
    setSubmitting(true)
    try {
      await putConfigPatch({})
      onDone()
    } catch {
      toast.error(t('toast.skipFailed'))
    } finally {
      setSubmitting(false)
    }
  }

  /** 完成 = 写入向导收集的最小 patch（PUT /api/config 深度合并语义） */
  const finish = async () => {
    if (submitting) return
    setSubmitting(true)
    try {
      await putConfigPatch({
        general: { language },
        pipeline: { workspace_dir: workspaceDir.trim() },
        server: { port: portNum },
      })
      setAppLanguage(language)
      onDone()
    } catch {
      toast.error(t('toast.saveFailed'), {
        description: t('toast.saveFailedDescription'),
      })
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-background p-4">
      <div className="w-full max-w-xl">
        {/* 品牌 + 步骤条 */}
        <div className="mb-6 text-center">
          <p className="text-2xl font-bold tracking-tight">EntryPoint</p>
          <ol className="mt-4 flex items-center justify-center gap-2 text-xs text-muted-foreground">
            {STEP_KEYS.map((key, i) => (
              <li key={key} className="flex items-center gap-2">
                <span
                  className={cn(
                    'rounded-full px-2 py-0.5',
                    i === step
                      ? 'bg-primary text-primary-foreground'
                      : i < step
                        ? 'bg-primary/15 text-primary'
                        : 'bg-muted',
                  )}
                >
                  {i + 1}
                </span>
                <span className={cn(i === step && 'text-foreground')}>
                  {t(key)}
                </span>
                {i < STEP_KEYS.length - 1 && <span aria-hidden>·</span>}
              </li>
            ))}
          </ol>
        </div>

        <Card>
          <CardHeader>
            <CardTitle>{t('title')}</CardTitle>
            <CardDescription>{t('subtitle')}</CardDescription>
          </CardHeader>
          <CardContent>
            {step === 0 && (
              <div className="space-y-3">
                <ul className="list-inside list-disc space-y-1.5 text-sm text-muted-foreground">
                  <li>{t('welcome.feature1')}</li>
                  <li>{t('welcome.feature2')}</li>
                  <li>{t('welcome.feature3')}</li>
                </ul>
                <div className="flex justify-end pt-2">
                  <Button onClick={() => setStep(1)}>{t('welcome.start')}</Button>
                </div>
              </div>
            )}

            {step === 1 && (
              <div className="space-y-3">
                <p className="text-sm text-muted-foreground">
                  {t('language.description')}
                </p>
                <div className="grid grid-cols-2 gap-3">
                  {LANGUAGE_OPTIONS.map((opt) => (
                    <button
                      key={opt.value}
                      type="button"
                      onClick={() => selectLanguage(opt.value)}
                      className={cn(
                        'rounded-md border px-4 py-3 text-sm font-medium transition-colors',
                        language === opt.value
                          ? 'border-primary bg-primary/10 text-foreground'
                          : 'border-input text-muted-foreground hover:bg-accent hover:text-accent-foreground',
                      )}
                    >
                      {opt.label}
                    </button>
                  ))}
                </div>
              </div>
            )}

            {step === 2 && (
              <div className="space-y-3">
                <p className="text-sm text-muted-foreground">
                  {t('workspace.description')}
                </p>
                <div className="space-y-1.5">
                  <p className="text-xs text-muted-foreground">
                    {t('workspace.rootLabel')}:{' '}
                    <code className="rounded bg-muted px-1 py-0.5">{status.root}</code>
                  </p>
                  <label className="text-sm font-medium" htmlFor="onboarding-workspace">
                    {t('workspace.label')}
                  </label>
                  <Input
                    id="onboarding-workspace"
                    value={workspaceDir}
                    onChange={(e) => setWorkspaceDir(e.target.value)}
                    placeholder={status.workspace_dir_raw}
                  />
                  <p className="text-xs text-muted-foreground">
                    {t('workspace.resolved')}:{' '}
                    <code className="rounded bg-muted px-1 py-0.5">
                      {workspaceTouched
                        ? previewResolve(status.root, workspaceDir.trim())
                        : status.workspace_dir}
                    </code>
                  </p>
                  {!workspaceValid && (
                    <p className="text-xs text-destructive">{t('workspace.required')}</p>
                  )}
                </div>
              </div>
            )}

            {step === 3 && (
              <div className="space-y-3">
                <p className="text-sm text-muted-foreground">
                  {t('port.description')}
                </p>
                <div className="space-y-1.5">
                  <label className="text-sm font-medium" htmlFor="onboarding-port">
                    {t('port.label')}
                  </label>
                  <Input
                    id="onboarding-port"
                    type="number"
                    min={1}
                    max={65535}
                    value={port}
                    onChange={(e) => setPort(e.target.value)}
                  />
                  {!portValid && (
                    <p className="text-xs text-destructive">{t('port.invalid')}</p>
                  )}
                  <p className="text-xs text-muted-foreground">{t('port.restartHint')}</p>
                </div>
              </div>
            )}

            {step === 4 && (
              <div className="space-y-3">
                <p className="text-sm text-muted-foreground">{t('done.summary')}</p>
                <dl className="space-y-1.5 rounded-md border p-3 text-sm">
                  <div className="flex justify-between gap-4">
                    <dt className="text-muted-foreground">{t('step.language')}</dt>
                    <dd>
                      {LANGUAGE_OPTIONS.find((o) => o.value === language)?.label ??
                        language}
                    </dd>
                  </div>
                  <div className="flex justify-between gap-4">
                    <dt className="text-muted-foreground">{t('workspace.label')}</dt>
                    <dd className="text-right">
                      <code className="rounded bg-muted px-1 py-0.5">
                        {workspaceTouched
                          ? previewResolve(status.root, workspaceDir.trim())
                          : status.workspace_dir}
                      </code>
                    </dd>
                  </div>
                  <div className="flex justify-between gap-4">
                    <dt className="text-muted-foreground">{t('port.label')}</dt>
                    <dd>{portNum}</dd>
                  </div>
                </dl>
                <p className="text-xs text-muted-foreground">{t('done.importHint')}</p>
                <div className="flex justify-end pt-2">
                  <Button onClick={finish} disabled={submitting || !workspaceValid || !portValid}>
                    {t('done.finish')}
                  </Button>
                </div>
              </div>
            )}
          </CardContent>
        </Card>

        {/* 底部导航：非欢迎/完成步显示上一步/下一步；全程可跳过（零强制） */}
        <div className="mt-4 flex items-center justify-between">
          <Button variant="ghost" size="sm" onClick={skip} disabled={submitting}>
            {t('action.skip')}
          </Button>
          <p className="max-w-[16rem] text-right text-xs text-muted-foreground">
            {t('footer.skipHint')}
          </p>
          <div className="flex gap-2">
            {step > 0 && (
              <Button
                variant="outline"
                size="sm"
                onClick={() => setStep((s) => s - 1)}
                disabled={submitting}
              >
                {t('action.back')}
              </Button>
            )}
            {step >= 1 && step < 3 && (
              <Button
                size="sm"
                onClick={() => setStep((s) => s + 1)}
                disabled={submitting || (step === 2 && !workspaceValid)}
              >
                {t('action.next')}
              </Button>
            )}
            {step === 3 && (
              <Button
                size="sm"
                onClick={() => setStep(4)}
                disabled={submitting || !portValid}
              >
                {t('action.next')}
              </Button>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}
