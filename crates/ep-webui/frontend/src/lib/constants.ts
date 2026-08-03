import i18n from '@/i18n'

/**
 * 模块级翻译助手：静态元数据（常量）无法使用 React Hook，
 * 在读取时按当前语言即时解析，语言切换后重渲染即可生效。
 * 跨命名空间键使用全限定形式（"common:xxx"）。
 */
function t(key: string): string {
  return i18n.t(key) as string
}

/** 模块分类 → i18n 键（components 命名空间） */
export const CATEGORY_LABELS: Record<string, string> = {
  asr: 'components:category.asr',
  tts: 'components:category.tts',
  denoise: 'components:category.denoise',
  ocr: 'components:category.ocr',
  image: 'components:category.image',
  video: 'components:category.video',
  audio: 'components:category.audio',
  translate: 'components:category.translate',
  llm: 'components:category.llm',
  other: 'components:category.other',
}

/** 获取分类的当前语言标签，未知分类原样返回 */
export function categoryLabel(category: string): string {
  const key = CATEGORY_LABELS[category.toLowerCase()]
  return key ? t(key) : category
}

/** 状态元信息：i18n 标签键 + 颜色类名 */
export interface StatusMeta {
  /** 状态标签的 i18n 键（common 命名空间）；null 表示 label 为后端原值透传 */
  labelKey: string | null
  /** 兼容字段：已知状态经 labelKey 按当前语言即时翻译，未知状态为原值透传 */
  label: string
  /** 状态圆点颜色 */
  dot: string
  /** 徽章配色（背景/文字/边框） */
  badge: string
  /** 是否处于过渡态（用于脉冲动画） */
  transitional: boolean
}

/**
 * 运行状态 → 元信息。
 * 状态值来自后端 status / service_status 字段，统一小写蛇形命名。
 * label 为 getter，读取时按当前语言解析；消费方可渐进迁移到 t(labelKey)。
 */
export const STATUS_COLORS: Record<string, StatusMeta> = {
  running: {
    labelKey: 'common:status.running',
    get label() {
      return t('common:status.running')
    },
    dot: 'bg-status-running',
    badge: 'bg-status-running/15 text-status-running border-status-running/30',
    transitional: false,
  },
  stopped: {
    labelKey: 'common:status.stopped',
    get label() {
      return t('common:status.stopped')
    },
    dot: 'bg-status-stopped',
    badge: 'bg-muted text-muted-foreground border-border',
    transitional: false,
  },
  starting: {
    labelKey: 'common:status.starting',
    get label() {
      return t('common:status.starting')
    },
    dot: 'bg-status-starting',
    badge: 'bg-status-starting/15 text-status-starting border-status-starting/30',
    transitional: true,
  },
  preparing: {
    labelKey: 'common:status.preparing',
    get label() {
      return t('common:status.preparing')
    },
    dot: 'bg-status-preparing',
    badge:
      'bg-status-preparing/15 text-status-preparing border-status-preparing/30',
    transitional: true,
  },
  error: {
    labelKey: 'common:status.error',
    get label() {
      return t('common:status.error')
    },
    dot: 'bg-status-error',
    badge: 'bg-status-error/15 text-status-error border-status-error/30',
    transitional: false,
  },
  not_ready: {
    labelKey: 'common:status.notReady',
    get label() {
      return t('common:status.notReady')
    },
    dot: 'bg-status-notready',
    badge: 'bg-muted text-muted-foreground border-border',
    transitional: false,
  },
}

const FALLBACK_STATUS: StatusMeta = {
  labelKey: 'common:status.unknown',
  get label() {
    return t('common:status.unknown')
  },
  dot: 'bg-muted-foreground',
  badge: 'bg-muted text-muted-foreground border-border',
  transitional: false,
}

/** 获取状态的元信息，自动归一化（小写、去空白、notready → not_ready） */
export function statusMeta(status: string | null | undefined): StatusMeta {
  if (!status) return FALLBACK_STATUS
  const key = status
    .trim()
    .toLowerCase()
    .replace(/\s+/g, '_')
    .replace('notready', 'not_ready')
  // 未知状态：labelKey 置 null，label 原样透传（与 tasks.tsx 的 labelKey 约定一致）
  return (
    STATUS_COLORS[key] ?? { ...FALLBACK_STATUS, labelKey: null, label: status }
  )
}
