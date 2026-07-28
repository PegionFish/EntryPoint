/** 模块分类 → 中文标签 */
export const CATEGORY_LABELS: Record<string, string> = {
  asr: '语音识别',
  tts: '语音合成',
  denoise: '降噪',
  ocr: '文字识别',
  image: '图像处理',
  video: '视频处理',
  audio: '音频处理',
  translate: '机器翻译',
  llm: '大语言模型',
  other: '其他',
}

/** 获取分类的中文标签，未知分类原样返回 */
export function categoryLabel(category: string): string {
  return CATEGORY_LABELS[category.toLowerCase()] ?? category
}

/** 状态元信息：中文标签 + 颜色类名 */
export interface StatusMeta {
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
 */
export const STATUS_COLORS: Record<string, StatusMeta> = {
  running: {
    label: '运行中',
    dot: 'bg-status-running',
    badge: 'bg-status-running/15 text-status-running border-status-running/30',
    transitional: false,
  },
  stopped: {
    label: '已停止',
    dot: 'bg-status-stopped',
    badge: 'bg-muted text-muted-foreground border-border',
    transitional: false,
  },
  starting: {
    label: '启动中',
    dot: 'bg-status-starting',
    badge: 'bg-status-starting/15 text-status-starting border-status-starting/30',
    transitional: true,
  },
  preparing: {
    label: '准备中',
    dot: 'bg-status-preparing',
    badge:
      'bg-status-preparing/15 text-status-preparing border-status-preparing/30',
    transitional: true,
  },
  error: {
    label: '错误',
    dot: 'bg-status-error',
    badge: 'bg-status-error/15 text-status-error border-status-error/30',
    transitional: false,
  },
  not_ready: {
    label: '未就绪',
    dot: 'bg-status-notready',
    badge: 'bg-muted text-muted-foreground border-border',
    transitional: false,
  },
}

const FALLBACK_STATUS: StatusMeta = {
  label: '未知',
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
  return STATUS_COLORS[key] ?? { ...FALLBACK_STATUS, label: status }
}
