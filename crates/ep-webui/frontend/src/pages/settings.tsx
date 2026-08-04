import {
  useEffect,
  useRef,
  useState,
  type ComponentProps,
  type ReactNode,
} from 'react'
import {
  Cpu,
  Database,
  GitBranch,
  Globe,
  Loader2,
  Network,
  Package,
  Plus,
  RotateCcw,
  Save,
  Server,
  Settings2,
  SlidersHorizontal,
  TerminalSquare,
  TriangleAlert,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import type { AppConfig } from '@/api/types'
import { normalizeLanguage, setAppLanguage } from '@/i18n'
import { adoptTheme, useThemeStore } from '@/store/theme'
import { PageContainer } from '@/components/layout/page-container'
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useConfig, type AppConfigExt } from '@/hooks/use-config'
import { cn } from '@/lib/utils'

/* ── 本地表单小组件 ─────────────────────────────────────────── */

function Section({
  icon: Icon,
  title,
  description,
  children,
}: {
  icon: typeof Server
  title: string
  description: string
  children: ReactNode
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base font-semibold">
          <Icon className="size-4 text-primary" />
          {title}
        </CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-x-6 gap-y-5 sm:grid-cols-2">
        {children}
      </CardContent>
    </Card>
  )
}

function Field({
  label,
  description,
  error,
  className,
  field,
  children,
}: {
  label: string
  description?: string
  /** 内联校验错误（红字提示） */
  error?: string
  className?: string
  /** 校验锚点（data-field），保存失败时用于滚动定位 */
  field?: string
  children: ReactNode
}) {
  return (
    <div className={cn('space-y-2', className)} data-field={field}>
      <div>
        <div className="text-sm font-medium">{label}</div>
        {description && (
          <div className="mt-0.5 text-xs text-muted-foreground">
            {description}
          </div>
        )}
      </div>
      {children}
      {error && <div className="text-xs text-status-error">{error}</div>}
    </div>
  )
}

function SwitchRow({
  label,
  description,
  className,
  ...props
}: {
  label: string
  description?: string
  className?: string
} & Omit<ComponentProps<typeof Switch>, 'className'>) {
  return (
    <div
      className={cn(
        'flex items-center justify-between gap-4 rounded-md border border-border px-4 py-3',
        className,
      )}
    >
      <div className="min-w-0">
        <div className="text-sm font-medium">{label}</div>
        {description && (
          <div className="mt-0.5 text-xs text-muted-foreground">
            {description}
          </div>
        )}
      </div>
      <Switch {...props} className="shrink-0" />
    </div>
  )
}

function NumberField({
  value,
  onValueChange,
  min,
  max,
  invalid,
}: {
  value: number
  onValueChange: (v: number) => void
  min?: number
  max?: number
  /** 校验未通过（红色边框提示） */
  invalid?: boolean
}) {
  // 本地文本镜像：容忍清空等过渡性无效输入，只在输入为合法数字时回写配置
  const [text, setText] = useState(() =>
    Number.isFinite(value) ? String(value) : '',
  )

  // 外部值变化（重置、重新加载、保存回写）时同步显示
  useEffect(() => {
    setText(Number.isFinite(value) ? String(value) : '')
  }, [value])

  return (
    <Input
      type="number"
      value={text}
      min={min}
      max={max}
      aria-invalid={invalid || undefined}
      onChange={(e) => {
        setText(e.target.value)
        const v = e.target.valueAsNumber
        // P2-52：非法 / NaN 输入保留原值，不再写 0
        if (!Number.isNaN(v)) onValueChange(v)
      }}
      onBlur={() => {
        // 失焦时回退显示为实际配置值，避免无效内容造成误导
        setText(Number.isFinite(value) ? String(value) : '')
      }}
      className="font-mono"
    />
  )
}

/* ── 表单校验 ─────────────────────────────────────────────────── */

/** 字段级校验错误（对应 key 缺失即无错误） */
interface ValidationErrors {
  server_port?: string
  range_start?: string
  range_end?: string
  /** 交叉校验：起始端口必须小于结束端口（P2-53） */
  ports_range?: string
  http_proxy?: string
  https_proxy?: string
}

/** 翻译函数最小签名：供模块级校验函数使用，与 react-i18next 的 t 类型解耦 */
type TranslateFn = (key: string) => string

/** 端口必须为 1–65535 的整数 */
function portInvalid(value: number): boolean {
  return !(Number.isInteger(value) && value >= 1 && value <= 65535)
}

/** 代理地址非空时必须以 http:// 或 https:// 开头 */
function proxyInvalid(value: string): boolean {
  const v = value.trim()
  return v !== '' && !/^https?:\/\//.test(v)
}

/** 对当前表单做整体校验，返回全部错误（错误文案经 i18n 翻译） */
function validateConfig(config: AppConfig, t: TranslateFn): ValidationErrors {
  const errors: ValidationErrors = {}
  if (portInvalid(config.server.port))
    errors.server_port = t('validation.port')
  if (portInvalid(config.ports.range_start))
    errors.range_start = t('validation.port')
  if (portInvalid(config.ports.range_end))
    errors.range_end = t('validation.port')
  if (
    !errors.range_start &&
    !errors.range_end &&
    config.ports.range_start >= config.ports.range_end
  ) {
    errors.ports_range = t('validation.portRange')
  }
  if (proxyInvalid(config.network?.http_proxy ?? ''))
    errors.http_proxy = t('validation.proxy')
  if (proxyInvalid(config.network?.https_proxy ?? ''))
    errors.https_proxy = t('validation.proxy')
  return errors
}

/** 校验失败时的滚动定位顺序：首个错误 → 页内锚点（data-field） */
const VALIDATION_ORDER: { key: keyof ValidationErrors; anchor: string }[] = [
  { key: 'server_port', anchor: 'server-port' },
  { key: 'range_start', anchor: 'range-start' },
  { key: 'range_end', anchor: 'range-end' },
  { key: 'ports_range', anchor: 'range-end' },
  { key: 'http_proxy', anchor: 'http-proxy' },
  { key: 'https_proxy', anchor: 'https-proxy' },
]

function scrollToFirstError(errors: ValidationErrors) {
  const first = VALIDATION_ORDER.find(({ key }) => errors[key] !== undefined)
  if (!first) return
  document
    .querySelector(`[data-field="${first.anchor}"]`)
    ?.scrollIntoView({ behavior: 'smooth', block: 'center' })
}

/* ── 设置页 ──────────────────────────────────────────────────── */

export function SettingsPage() {
  const { t } = useTranslation('settings')
  /** t 的轻量包装：供模块级校验函数使用（隔离 react-i18next 类型） */
  const tr: TranslateFn = (key) => t(key)
  const {
    config,
    setConfig,
    save,
    persistPartial,
    reload,
    loading,
    saving,
    error,
    dirty,
  } = useConfig()
  /** 开启「允许公网访问」前的安全确认对话框 */
  const [publicDialogOpen, setPublicDialogOpen] = useState(false)
  /** active_models 新增行草稿（确认前不进入配置 draft） */
  const [newActiveModel, setNewActiveModel] = useState<{
    moduleId: string
    modelId: string
  } | null>(null)
  /** 实时字段校验错误（由当前表单内容派生） */
  const errors: ValidationErrors = config ? validateConfig(config, tr) : {}

  /** 主题 store 当前值（作为订阅触发器：顶栏/外部入口切换时驱动草稿同步） */
  const storeTheme = useThemeStore((s) => s.theme)
  /** 跟踪 loading 跳变：仅在"加载刚完成"时刻执行服务器优先对齐 */
  const prevLoadingRef = useRef(loading)

  /**
   * 主题三端同步（P2-2）——服务器优先：配置加载/重载（首次进入、重置）完成
   * 的那一刻，本地主题与服务器不一致则以服务器为准（此刻草稿 == 服务器快照，
   * 方向明确）。adoptTheme 只本地应用不回写，避免把真源值再写回去。
   */
  useEffect(() => {
    const justLoaded = prevLoadingRef.current && !loading
    prevLoadingRef.current = loading
    if (!justLoaded || !config) return
    const serverTheme = config.general.theme
    if (
      (serverTheme === 'dark' || serverTheme === 'light') &&
      useThemeStore.getState().theme !== serverTheme
    ) {
      adoptTheme(serverTheme)
    }
  }, [loading, config])

  /**
   * 主题三端同步（P2-2）——草稿跟随 store：设置页打开期间顶栏等其他入口
   * 切换主题时，同步草稿中的 theme 字段，避免保存按钮把旧值写回服务器。
   * effect 内读取 getState() 取最新值，规避同批提交中的过期闭包。
   */
  useEffect(() => {
    if (!config) return
    const current = useThemeStore.getState().theme
    if (config.general.theme !== current) {
      setConfig(
        (prev) =>
          ({
            ...prev,
            general: { ...prev.general, theme: current },
          }) as AppConfigExt,
      )
    }
  }, [storeTheme, config, setConfig])

  /** 局部更新某个配置分区 */
  function patchSection<K extends keyof AppConfigExt>(
    key: K,
    patch: Partial<AppConfigExt[K]>,
  ) {
    setConfig(
      (prev) =>
        ({ ...prev, [key]: { ...prev[key], ...patch } }) as AppConfigExt,
    )
  }

  /** active_models 单键写入（高级区 KV 编辑，§5.2 单槽位） */
  function setActiveModel(moduleId: string, modelId: string) {
    setConfig(
      (prev) =>
        ({
          ...prev,
          active_models: { ...(prev.active_models ?? {}), [moduleId]: modelId },
        }) as AppConfigExt,
    )
  }

  /**
   * 界面语言切换：即时生效 + 立即持久化（不等保存按钮）。
   * 持久化以服务器最新配置为 PUT 基线、只覆盖 language 字段，
   * 不会把页面上其他未保存的 draft 改动一起提交。
   */
  async function handleLanguageChange(value: string) {
    const lang = normalizeLanguage(value)
    // 1) 同步 draft，Select 选中态立即可见
    patchSection('general', { language: lang })
    // 2) i18n 立即生效（changeLanguage + localStorage + <html lang> + 标题）
    setAppLanguage(lang)
    // 3) 立即持久化到服务器（全局唯一真源；PUT 合并单键，§8.2）
    const ok = await persistPartial({ general: { language: lang } })
    if (ok) {
      toast.success(t('toast.languageSaved'))
    } else {
      toast.error(t('toast.languageSaveFailed'), {
        description: t('toast.languageSaveFailedDescription'),
      })
    }
  }

  async function handleSave() {
    if (!config) return
    // 校验未通过：阻止保存、汇总报错并滚动到首个出错字段
    const validationErrors = validateConfig(config, tr)
    const messages = [
      ...new Set(
        Object.values(validationErrors).filter((m): m is string => Boolean(m)),
      ),
    ]
    if (messages.length > 0) {
      toast.error(t('toast.validationFailed'), {
        description: messages.join('; '),
      })
      scrollToFirstError(validationErrors)
      return
    }
    const toastId = toast.loading(t('toast.saving'))
    const resp = await save()
    if (resp) {
      if (resp.requires_restart) {
        // §8.2：服务端判定本次改动触及重启敏感项（P2-15 重启引导）
        toast.warning(t('toast.savedRestart'), {
          id: toastId,
          description: t('toast.savedRestartDescription'),
        })
      } else {
        toast.success(t('toast.saved'), { id: toastId })
      }
    } else {
      toast.error(t('toast.saveFailed'), {
        id: toastId,
        description: t('toast.saveFailedHint'),
      })
    }
  }

  async function handleReset() {
    await reload()
    toast.info(t('toast.reset'))
  }

  function handleAllowPublic(checked: boolean) {
    if (checked) {
      // 开启公网访问需先经安全确认，确认前不写入配置
      setPublicDialogOpen(true)
    } else {
      patchSection('server', { allow_public: false })
    }
  }

  return (
    <PageContainer
      title={t('title')}
      description={t('description')}
      actions={
        <>
          {dirty && (
            <span className="flex items-center gap-1.5 text-xs text-status-preparing">
              <span className="size-1.5 animate-pulse rounded-full bg-status-preparing" />
              {t('common:tip.unsavedChanges')}
            </span>
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={() => void handleReset()}
            disabled={loading || saving || !dirty}
          >
            <RotateCcw className="size-3.5" />
            {t('action.reset')}
          </Button>
          <Button
            size="sm"
            onClick={() => void handleSave()}
            disabled={loading || saving || !dirty}
          >
            {saving ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <Save className="size-3.5" />
            )}
            {t('common:action.save')}
          </Button>
        </>
      }
    >
      {loading || !config ? (
        <div className="mx-auto max-w-4xl space-y-6">
          {Array.from({ length: 3 }).map((_, i) => (
            <Skeleton key={i} className="h-48 rounded-lg" />
          ))}
        </div>
      ) : (
        <div className="mx-auto max-w-4xl space-y-6">
          {error && (
            <div className="flex items-center gap-2 rounded-lg border border-status-error/30 bg-status-error/10 px-4 py-3 text-sm text-status-error">
              <TriangleAlert className="size-4 shrink-0" />
              <span className="min-w-0 flex-1 truncate">{error}</span>
            </div>
          )}

          {/* ── 服务器 ── */}
          <Section
            icon={Server}
            title={t('server.title')}
            description={t('server.description')}
          >
            <Field
              label={t('server.host')}
              description={t('server.hostDescription')}
            >
              <Input
                value={config.server.host}
                onChange={(e) =>
                  patchSection('server', { host: e.target.value })
                }
                placeholder="0.0.0.0"
                className="font-mono text-xs"
              />
            </Field>
            <Field
              label={t('server.port')}
              description={t('server.portDescription')}
              field="server-port"
              error={errors.server_port}
            >
              <NumberField
                value={config.server.port}
                onValueChange={(v) => patchSection('server', { port: v })}
                min={1}
                max={65535}
                invalid={Boolean(errors.server_port)}
              />
            </Field>
            <SwitchRow
              className="sm:col-span-2"
              label={t('server.allowPublic')}
              description={t('server.allowPublicDescription')}
              checked={config.server.allow_public}
              onCheckedChange={handleAllowPublic}
            />
          </Section>

          {/* ── 通用 ── */}
          <Section
            icon={Settings2}
            title={t('general.title')}
            description={t('general.description')}
          >
            <Field label={t('general.language')}>
              <Select
                value={normalizeLanguage(config.general.language)}
                onValueChange={(v) => void handleLanguageChange(v)}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="zh-CN">简体中文</SelectItem> {/* i18n-exempt: native label（语言选项固定以本族语显示，i18n 惯例，不进翻译文件） */}
                  <SelectItem value="en">English</SelectItem> {/* i18n-exempt: native label（同上） */}
                </SelectContent>
              </Select>
            </Field>
            <Field label={t('common:label.theme')}>
              <Select
                value={config.general.theme}
                onValueChange={(v) => {
                  patchSection('general', { theme: v })
                  // 主题三端同步（P2-2）：store.setTheme 本地应用并即时回写服务器，
                  // 与顶栏切换共享同一视觉状态与同一持久化路径
                  if (v === 'dark' || v === 'light') {
                    useThemeStore.getState().setTheme(v)
                  }
                }}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="dark">
                    {t('common:label.dark')}
                  </SelectItem>
                  <SelectItem value="light">
                    {t('common:label.light')}
                  </SelectItem>
                  {/* P2-46：「跟随系统」已移除，theme store 仅实现 dark/light */}
                </SelectContent>
              </Select>
            </Field>
            <Field
              label={t('general.logLevel')}
              // P2-1 处置：daemon tracing 订阅尚未读取本项（接线需 main.rs，
              // 已提仲裁请求）；说明文案如实标注当前状态，避免误导
              description={t('general.logLevelDescription')}
            >
              <Select
                value={config.general.log_level}
                onValueChange={(v) =>
                  patchSection('general', { log_level: v })
                }
              >
                <SelectTrigger className="w-full font-mono text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {['trace', 'debug', 'info', 'warn', 'error'].map((lv) => (
                    <SelectItem key={lv} value={lv} className="font-mono">
                      {lv}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <SwitchRow
              label={t('general.checkUpdates')}
              // P1-10：daemon 自身更新检查尚无消费者（本期无人认领接线），
              // 保留开关 + 文案如实标注后端待接，避免误导
              description={t('general.checkUpdatesPending')}
              checked={config.general.check_updates}
              onCheckedChange={(v) =>
                patchSection('general', { check_updates: v })
              }
            />
          </Section>

          {/* ── 计算 ── */}
          <Section
            icon={Cpu}
            title={t('compute.title')}
            description={t('compute.description')}
          >
            <Field
              label={t('compute.strategy')}
              description={t('compute.strategyDescription')}
            >
              <Select
                value={config.compute.strategy}
                onValueChange={(v) => patchSection('compute', { strategy: v })}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="least_memory">
                    {t('strategy.leastMemory')}
                  </SelectItem>
                  <SelectItem value="round_robin">
                    {t('strategy.roundRobin')}
                  </SelectItem>
                  <SelectItem value="manual">
                    {t('strategy.manual')}
                  </SelectItem>
                </SelectContent>
              </Select>
            </Field>
            <Field
              label={t('compute.refreshInterval')}
              description={t('compute.refreshIntervalDescription')}
            >
              <NumberField
                value={config.compute.refresh_interval_secs}
                onValueChange={(v) =>
                  patchSection('compute', { refresh_interval_secs: v })
                }
                min={1}
                max={3600}
              />
            </Field>
            <SwitchRow
              className="sm:col-span-2"
              label={t('compute.allowOvercommit')}
              description={t('compute.allowOvercommitDescription')}
              checked={config.compute.allow_overcommit}
              onCheckedChange={(v) =>
                patchSection('compute', { allow_overcommit: v })
              }
            />
            {/* §8.3：共享 CUDA 库目录（Linux LD_LIBRARY_PATH / Windows PATH 注入） */}
            <Field
              label={t('compute.cudaLibsDir')}
              description={t('compute.cudaLibsDirDescription')}
              className="sm:col-span-2"
            >
              <Input
                value={config.compute.cuda_libs_dir ?? ''}
                onChange={(e) =>
                  patchSection('compute', { cuda_libs_dir: e.target.value })
                }
                placeholder="runtime/cuda-libs"
                className="font-mono text-xs"
              />
            </Field>
          </Section>

          {/* ── 端口 ── */}
          <Section
            icon={Network}
            title={t('ports.title')}
            description={t('ports.description')}
          >
            <Field
              label={t('ports.rangeStart')}
              field="range-start"
              error={errors.range_start}
            >
              <NumberField
                value={config.ports.range_start}
                onValueChange={(v) =>
                  patchSection('ports', { range_start: v })
                }
                min={1024}
                max={65535}
                invalid={Boolean(errors.range_start)}
              />
            </Field>
            <Field
              label={t('ports.rangeEnd')}
              field="range-end"
              error={errors.range_end ?? errors.ports_range}
            >
              <NumberField
                value={config.ports.range_end}
                onValueChange={(v) => patchSection('ports', { range_end: v })}
                min={1024}
                max={65535}
                invalid={Boolean(errors.range_end ?? errors.ports_range)}
              />
            </Field>
          </Section>

          {/* ── 模型 ── */}
          <Section
            icon={Database}
            title={t('models.title')}
            description={t('models.description')}
          >
            <Field
              label={t('models.cacheDir')}
              description={t('models.cacheDirDescription')}
            >
              <Input
                value={config.models.cache_dir}
                onChange={(e) =>
                  patchSection('models', { cache_dir: e.target.value })
                }
                placeholder="./models"
                className="font-mono text-xs"
              />
            </Field>
            <Field
              label={t('models.hfEndpoint')}
              description={t('models.hfEndpointDescription')}
            >
              <Input
                value={config.models.hf_endpoint}
                onChange={(e) =>
                  patchSection('models', { hf_endpoint: e.target.value })
                }
                placeholder="https://huggingface.co"
                className="font-mono text-xs"
              />
            </Field>
            <Field label={t('models.defaultSource')}>
              <Select
                value={config.models.default_source}
                onValueChange={(v) =>
                  patchSection('models', { default_source: v })
                }
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="huggingface">HuggingFace</SelectItem>
                  <SelectItem value="modelscope">ModelScope</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            <Field label={t('models.maxConcurrentDownloads')}>
              <NumberField
                value={config.models.max_concurrent_downloads}
                onValueChange={(v) =>
                  patchSection('models', { max_concurrent_downloads: v })
                }
                min={1}
                max={16}
              />
            </Field>
            <Field
              label={t('models.cachePaths')}
              description={t('models.cachePathsDescription')}
              className="sm:col-span-2"
            >
              <Input
                value={config.models.cache_paths.join(', ')}
                onChange={(e) =>
                  patchSection('models', {
                    cache_paths: e.target.value
                      .split(',')
                      .map((p) => p.trim())
                      .filter(Boolean),
                  })
                }
                placeholder="/data/models, /mnt/cache/models"
                className="font-mono text-xs"
              />
            </Field>
          </Section>

          {/* ── 网络与代理 ── */}
          <Section
            icon={Globe}
            title={t('network.title')}
            description={t('network.description')}
          >
            <Field
              label={t('network.httpProxy')}
              field="http-proxy"
              error={errors.http_proxy}
            >
              <Input
                value={config.network?.http_proxy ?? ''}
                onChange={(e) =>
                  patchSection('network', { http_proxy: e.target.value })
                }
                placeholder="http://127.0.0.1:7890"
                aria-invalid={Boolean(errors.http_proxy) || undefined}
                className="font-mono text-xs"
              />
            </Field>
            <Field
              label={t('network.httpsProxy')}
              field="https-proxy"
              error={errors.https_proxy}
            >
              <Input
                value={config.network?.https_proxy ?? ''}
                onChange={(e) =>
                  patchSection('network', { https_proxy: e.target.value })
                }
                placeholder="http://127.0.0.1:7890"
                aria-invalid={Boolean(errors.https_proxy) || undefined}
                className="font-mono text-xs"
              />
            </Field>
            <Field
              label={t('network.noProxy')}
              description={t('network.noProxyDescription')}
              className="sm:col-span-2"
            >
              <Input
                value={config.network?.no_proxy ?? ''}
                onChange={(e) =>
                  patchSection('network', { no_proxy: e.target.value })
                }
                placeholder="localhost,127.0.0.1"
                className="font-mono text-xs"
              />
            </Field>
          </Section>

          {/* ── Python ── */}
          <Section
            icon={TerminalSquare}
            title={t('python.title')}
            description={t('python.description')}
          >
            <Field label={t('python.path')}>
              <Input
                value={config.python.path}
                onChange={(e) =>
                  patchSection('python', { path: e.target.value })
                }
                placeholder="python"
                className="font-mono text-xs"
              />
            </Field>
            <Field
              label={t('python.uvPath')}
              description={t('python.uvPathDescription')}
            >
              <Input
                value={config.python.uv_path}
                onChange={(e) =>
                  patchSection('python', { uv_path: e.target.value })
                }
                placeholder="uv"
                className="font-mono text-xs"
              />
            </Field>
            {/* §8.3：依赖栈统一（uv 缓存入应用根 → 与 venv 同盘硬链接去重） */}
            <Field
              label={t('python.uvCacheDir')}
              description={t('python.uvCacheDirDescription')}
            >
              <Input
                value={config.python.uv_cache_dir ?? ''}
                onChange={(e) =>
                  patchSection('python', { uv_cache_dir: e.target.value })
                }
                placeholder="runtime/.uv-cache"
                className="font-mono text-xs"
              />
            </Field>
            <Field
              label={t('python.constraints')}
              description={t('python.constraintsDescription')}
            >
              <Input
                value={config.python.constraints ?? ''}
                onChange={(e) =>
                  patchSection('python', { constraints: e.target.value })
                }
                placeholder="config/constraints.txt"
                className="font-mono text-xs"
              />
            </Field>
          </Section>

          {/* ── 整合包（§8.3）── */}
          <Section
            icon={Package}
            title={t('packs.title')}
            description={t('packs.description')}
          >
            <Field
              label={t('packs.stagingDir')}
              description={t('packs.stagingDirDescription')}
              className="sm:col-span-2"
            >
              <Input
                value={config.packs?.staging_dir ?? ''}
                onChange={(e) =>
                  patchSection('packs', { staging_dir: e.target.value })
                }
                placeholder=".pack-staging"
                className="font-mono text-xs"
              />
            </Field>
          </Section>

          {/* ── 管线 ── */}
          <Section
            icon={GitBranch}
            title={t('pipeline.title')}
            description={t('pipeline.description')}
          >
            <Field
              label={t('pipeline.maxParallel')}
              description={t('pipeline.maxParallelDescription')}
            >
              <NumberField
                value={config.pipeline.max_parallel}
                onValueChange={(v) =>
                  patchSection('pipeline', { max_parallel: v })
                }
                min={1}
                max={64}
              />
            </Field>
            <Field
              label={t('pipeline.defaultTimeout')}
              description={t('pipeline.defaultTimeoutDescription')}
            >
              <NumberField
                value={config.pipeline.default_timeout_secs}
                onValueChange={(v) =>
                  patchSection('pipeline', { default_timeout_secs: v })
                }
                min={1}
              />
            </Field>
            <Field
              label={t('pipeline.workspaceDir')}
              description={t('pipeline.workspaceDirDescription')}
            >
              <Input
                value={config.pipeline.workspace_dir}
                onChange={(e) =>
                  patchSection('pipeline', { workspace_dir: e.target.value })
                }
                placeholder="./workspace"
                className="font-mono text-xs"
              />
            </Field>
            <SwitchRow
              label={t('pipeline.keepWorkspace')}
              // P2-1 核查：本项当前无后端消费者（工作区清理未实现），
              // 标注实验性保留，避免"配置有效"的误导
              description={`${t('pipeline.keepWorkspaceDescription')} · ${t('pipeline.keepWorkspaceNote')}`}
              checked={config.pipeline.keep_workspace}
              onCheckedChange={(v) =>
                patchSection('pipeline', { keep_workspace: v })
              }
            />
          </Section>

          {/* ── 高级（§8.3 active_models 单槽位）── */}
          <Section
            icon={SlidersHorizontal}
            title={t('advanced.title')}
            description={t('advanced.description')}
          >
            <div className="space-y-3 sm:col-span-2">
              <div>
                <div className="text-sm font-medium">
                  {t('advanced.activeModels')}
                </div>
                <div className="mt-0.5 text-xs text-muted-foreground">
                  {t('advanced.activeModelsHint')}
                </div>
              </div>
              {Object.entries(config.active_models ?? {}).length === 0 &&
                !newActiveModel && (
                  <div className="rounded-md border border-dashed border-border px-4 py-3 text-xs text-muted-foreground">
                    {t('advanced.empty')}
                  </div>
                )}
              {Object.entries(config.active_models ?? {}).map(
                ([moduleId, modelId]) => (
                  <div
                    key={moduleId}
                    className="flex flex-col gap-2 sm:flex-row sm:items-center"
                  >
                    <Input
                      value={moduleId}
                      readOnly
                      title={t('advanced.keyReadOnly')}
                      className="flex-1 font-mono text-xs"
                    />
                    <span className="hidden text-xs text-muted-foreground sm:inline">
                      →
                    </span>
                    <Input
                      value={modelId}
                      onChange={(e) => setActiveModel(moduleId, e.target.value)}
                      placeholder="model-id"
                      aria-label={`${t('advanced.activeModels')} ${moduleId}`}
                      className="flex-1 font-mono text-xs"
                    />
                  </div>
                ),
              )}
              {newActiveModel ? (
                <div className="space-y-2 rounded-md border border-border px-3 py-3">
                  <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
                    <Input
                      value={newActiveModel.moduleId}
                      onChange={(e) =>
                        setNewActiveModel({
                          moduleId: e.target.value,
                          modelId: newActiveModel.modelId,
                        })
                      }
                      placeholder={t('advanced.moduleIdPlaceholder')}
                      aria-label={t('advanced.moduleIdPlaceholder')}
                      className="flex-1 font-mono text-xs"
                    />
                    <span className="hidden text-xs text-muted-foreground sm:inline">
                      →
                    </span>
                    <Input
                      value={newActiveModel.modelId}
                      onChange={(e) =>
                        setNewActiveModel({
                          moduleId: newActiveModel.moduleId,
                          modelId: e.target.value,
                        })
                      }
                      placeholder={t('advanced.modelIdPlaceholder')}
                      aria-label={t('advanced.modelIdPlaceholder')}
                      className="flex-1 font-mono text-xs"
                    />
                  </div>
                  <div className="flex justify-end gap-2">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setNewActiveModel(null)}
                    >
                      {t('common:action.cancel')}
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={
                        newActiveModel.moduleId.trim() === '' ||
                        newActiveModel.modelId.trim() === ''
                      }
                      onClick={() => {
                        setActiveModel(
                          newActiveModel.moduleId.trim(),
                          newActiveModel.modelId.trim(),
                        )
                        setNewActiveModel(null)
                      }}
                    >
                      {t('common:action.confirm')}
                    </Button>
                  </div>
                </div>
              ) : (
                <div>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() =>
                      setNewActiveModel({ moduleId: '', modelId: '' })
                    }
                  >
                    <Plus className="size-3.5" />
                    {t('advanced.addEntry')}
                  </Button>
                </div>
              )}
            </div>
          </Section>
        </div>
      )}

      {/* ── 公网访问安全确认 ── */}
      <Dialog
        open={publicDialogOpen}
        onOpenChange={(open) => {
          if (!open) setPublicDialogOpen(false)
        }}
      >
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2 text-status-preparing">
              <TriangleAlert className="size-4" />
              {t('publicDialog.title')}
            </DialogTitle>
            <DialogDescription asChild>
              <div className="space-y-2 pt-1 text-sm leading-relaxed">
                <p>{t('publicDialog.risk')}</p>
                <p>{t('publicDialog.noAuth')}</p>
                <p className="font-medium text-foreground">
                  {t('publicDialog.warning')}
                </p>
              </div>
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setPublicDialogOpen(false)}
            >
              {t('common:action.cancel')}
            </Button>
            <Button
              variant="destructive"
              onClick={() => {
                patchSection('server', { allow_public: true })
                setPublicDialogOpen(false)
              }}
            >
              {t('publicDialog.confirm')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  )
}
