import { useEffect, useState, type ComponentProps, type ReactNode } from 'react'
import {
  Cpu,
  Database,
  GitBranch,
  Globe,
  Loader2,
  Network,
  RotateCcw,
  Save,
  Server,
  Settings2,
  TerminalSquare,
  TriangleAlert,
} from 'lucide-react'
import { toast } from 'sonner'
import type { AppConfig } from '@/api/types'
import { useThemeStore } from '@/store/theme'
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
import { useConfig } from '@/hooks/use-config'
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

/** 端口必须为 1–65535 的整数 */
function portError(value: number): string | undefined {
  return Number.isInteger(value) && value >= 1 && value <= 65535
    ? undefined
    : '端口必须为 1–65535 的整数'
}

/** 代理地址非空时必须以 http:// 或 https:// 开头 */
function proxyError(value: string): string | undefined {
  const v = value.trim()
  return v === '' || /^https?:\/\//.test(v)
    ? undefined
    : '必须以 http:// 或 https:// 开头'
}

/** 对当前表单做整体校验，返回全部错误 */
function validateConfig(config: AppConfig): ValidationErrors {
  const errors: ValidationErrors = {}
  const serverPort = portError(config.server.port)
  if (serverPort) errors.server_port = serverPort
  const rangeStart = portError(config.ports.range_start)
  if (rangeStart) errors.range_start = rangeStart
  const rangeEnd = portError(config.ports.range_end)
  if (rangeEnd) errors.range_end = rangeEnd
  if (
    !rangeStart &&
    !rangeEnd &&
    config.ports.range_start >= config.ports.range_end
  ) {
    errors.ports_range = '起始端口必须小于结束端口'
  }
  const httpProxy = proxyError(config.network?.http_proxy ?? '')
  if (httpProxy) errors.http_proxy = httpProxy
  const httpsProxy = proxyError(config.network?.https_proxy ?? '')
  if (httpsProxy) errors.https_proxy = httpsProxy
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
  const { config, setConfig, save, reload, loading, saving, error, dirty } =
    useConfig()
  /** 开启「允许公网访问」前的安全确认对话框 */
  const [publicDialogOpen, setPublicDialogOpen] = useState(false)
  /** 实时字段校验错误（由当前表单内容派生） */
  const errors: ValidationErrors = config ? validateConfig(config) : {}

  /** 局部更新某个配置分区 */
  function patchSection<K extends keyof AppConfig>(
    key: K,
    patch: Partial<AppConfig[K]>,
  ) {
    setConfig(
      (prev) => ({ ...prev, [key]: { ...prev[key], ...patch } }) as AppConfig,
    )
  }

  async function handleSave() {
    if (!config) return
    // 校验未通过：阻止保存、汇总报错并滚动到首个出错字段
    const validationErrors = validateConfig(config)
    const messages = [
      ...new Set(
        Object.values(validationErrors).filter((m): m is string => Boolean(m)),
      ),
    ]
    if (messages.length > 0) {
      toast.error('配置校验未通过，请修正后再保存', {
        description: messages.join('；'),
      })
      scrollToFirstError(validationErrors)
      return
    }
    const toastId = toast.loading('正在保存配置…')
    const ok = await save()
    if (ok) {
      toast.success('配置已保存', {
        id: toastId,
        // P2-51：server.host / port 等改动需重启 daemon 才生效
        description: '服务器地址/端口等改动需重启服务后生效',
      })
    } else {
      toast.error('配置保存失败', {
        id: toastId,
        description: '请检查服务状态后重试',
      })
    }
  }

  async function handleReset() {
    await reload()
    toast.info('已恢复为上次保存的配置')
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
      title="设置"
      description="服务器、计算、模型与环境配置"
      actions={
        <>
          {dirty && (
            <span className="flex items-center gap-1.5 text-xs text-status-preparing">
              <span className="size-1.5 animate-pulse rounded-full bg-status-preparing" />
              未保存的更改
            </span>
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={() => void handleReset()}
            disabled={loading || saving || !dirty}
          >
            <RotateCcw className="size-3.5" />
            重置
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
            保存
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
            title="服务器"
            description="EntryPoint 服务的监听地址与访问控制"
          >
            <Field label="监听地址" description="0.0.0.0 表示监听所有网卡">
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
              label="端口"
              description="WebUI 与 API 服务端口"
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
              label="允许公网访问"
              description="关闭时仅允许私有网段与回环地址访问"
              checked={config.server.allow_public}
              onCheckedChange={handleAllowPublic}
            />
          </Section>

          {/* ── 通用 ── */}
          <Section
            icon={Settings2}
            title="通用"
            description="界面语言、主题与日志级别"
          >
            <Field label="界面语言">
              {/* P1-43：后端暂无 i18n，English 等选项实际无效，已移除；i18n 接入后再开放 */}
              <Select
                value={config.general.language}
                onValueChange={(v) => patchSection('general', { language: v })}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="zh">简体中文</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            <Field label="主题">
              <Select
                value={config.general.theme}
                onValueChange={(v) => {
                  patchSection('general', { theme: v })
                  // 同步本地主题 store：与顶栏切换共享同一视觉状态（W4-B 巡测发现不同步）
                  if (v === 'dark' || v === 'light') {
                    useThemeStore.getState().setTheme(v)
                  }
                }}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="dark">深色</SelectItem>
                  <SelectItem value="light">浅色</SelectItem>
                  {/* P2-46：「跟随系统」已移除，theme store 仅实现 dark/light */}
                </SelectContent>
              </Select>
            </Field>
            <Field label="日志级别">
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
              label="启动时检查更新"
              description="自动检测新版本并发出提示"
              checked={config.general.check_updates}
              onCheckedChange={(v) =>
                patchSection('general', { check_updates: v })
              }
            />
          </Section>

          {/* ── 计算 ── */}
          <Section
            icon={Cpu}
            title="计算"
            description="计算设备分配策略与资源监控"
          >
            <Field label="分配策略" description="模块启动时选择计算设备的策略">
              <Select
                value={config.compute.strategy}
                onValueChange={(v) => patchSection('compute', { strategy: v })}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="least_memory">最小显存优先</SelectItem>
                  <SelectItem value="round_robin">轮询</SelectItem>
                  <SelectItem value="manual">手动</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            <Field label="刷新间隔（秒）" description="设备状态轮询周期">
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
              label="允许显存超额"
              description="允许模块分配到显存不足的设备（可能导致加载失败）"
              checked={config.compute.allow_overcommit}
              onCheckedChange={(v) =>
                patchSection('compute', { allow_overcommit: v })
              }
            />
          </Section>

          {/* ── 端口 ── */}
          <Section
            icon={Network}
            title="端口"
            description="模块服务自动分配端口的可用范围"
          >
            <Field
              label="起始端口"
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
              label="结束端口"
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
            title="模型"
            description="模型缓存目录、下载源与本地搜索路径"
          >
            <Field label="缓存目录" description="模型文件的统一存放位置">
              <Input
                value={config.models.cache_dir}
                onChange={(e) =>
                  patchSection('models', { cache_dir: e.target.value })
                }
                placeholder="./models"
                className="font-mono text-xs"
              />
            </Field>
            <Field label="Hugging Face 镜像" description="下载 HF 模型时使用的镜像端点">
              <Input
                value={config.models.hf_endpoint}
                onChange={(e) =>
                  patchSection('models', { hf_endpoint: e.target.value })
                }
                placeholder="https://huggingface.co"
                className="font-mono text-xs"
              />
            </Field>
            <Field label="默认下载源">
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
            <Field label="最大并发下载数">
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
              label="本地缓存搜索路径"
              description="多个路径用英文逗号分隔，按优先级排序"
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
            title="网络与代理"
            description="示例：http://127.0.0.1:7890；留空 = 跟随系统环境变量；生效范围：模型下载、Python 依赖安装、模块进程"
          >
            <Field
              label="HTTP 代理（http_proxy）"
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
              label="HTTPS 代理（https_proxy）"
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
              label="代理排除列表（no_proxy）"
              description="不走代理的地址列表，默认 localhost,127.0.0.1"
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
            title="Python"
            description="模块运行所依赖 Python 与 uv 可执行文件路径"
          >
            <Field label="Python 路径">
              <Input
                value={config.python.path}
                onChange={(e) =>
                  patchSection('python', { path: e.target.value })
                }
                placeholder="python"
                className="font-mono text-xs"
              />
            </Field>
            <Field label="uv 路径" description="用于模块依赖安装与虚拟环境管理">
              <Input
                value={config.python.uv_path}
                onChange={(e) =>
                  patchSection('python', { uv_path: e.target.value })
                }
                placeholder="uv"
                className="font-mono text-xs"
              />
            </Field>
          </Section>

          {/* ── 管线 ── */}
          <Section
            icon={GitBranch}
            title="管线"
            description="管线执行的并发、超时与工作区配置"
          >
            <Field label="最大并行数" description="同时运行的管线任务上限">
              <NumberField
                value={config.pipeline.max_parallel}
                onValueChange={(v) =>
                  patchSection('pipeline', { max_parallel: v })
                }
                min={1}
                max={64}
              />
            </Field>
            <Field label="默认超时（秒）" description="单个管线任务的最长运行时间">
              <NumberField
                value={config.pipeline.default_timeout_secs}
                onValueChange={(v) =>
                  patchSection('pipeline', { default_timeout_secs: v })
                }
                min={1}
              />
            </Field>
            <Field label="工作区目录" description="管线运行时的中间文件存放位置">
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
              label="保留工作区"
              description="任务结束后保留中间文件，便于排查问题"
              checked={config.pipeline.keep_workspace}
              onCheckedChange={(v) =>
                patchSection('pipeline', { keep_workspace: v })
              }
            />
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
              ⚠️ 安全风险警告
            </DialogTitle>
            <DialogDescription asChild>
              <div className="space-y-2 pt-1 text-sm leading-relaxed">
                <p>
                  开启后，任何能访问此服务器 IP
                  的设备均可操作 EntryPoint，包括启停模块、修改配置。
                </p>
                <p>本项目不内置用户认证和传输加密。</p>
                <p className="font-medium text-foreground">
                  仅在您了解风险并有外部安全措施（如
                  VPN、防火墙规则）时开启。
                </p>
              </div>
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setPublicDialogOpen(false)}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={() => {
                patchSection('server', { allow_public: true })
                setPublicDialogOpen(false)
              }}
            >
              确认开启
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  )
}
