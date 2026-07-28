import { toast, type ExternalToast } from 'sonner'

/**
 * 全局 toast 默认项：右上角弹出，5 秒后自动消失。
 * 单项调用可通过 options 覆盖。
 */
export const TOAST_DEFAULTS: ExternalToast = {
  position: 'top-right',
  duration: 5000,
}

/** 成功提示（绿色对勾） */
export function toastSuccess(message: string, options?: ExternalToast) {
  return toast.success(message, { ...TOAST_DEFAULTS, ...options })
}

/** 错误提示（红色） */
export function toastError(message: string, options?: ExternalToast) {
  return toast.error(message, { ...TOAST_DEFAULTS, ...options })
}

/** 警告提示（黄色） */
export function toastWarning(message: string, options?: ExternalToast) {
  return toast.warning(message, { ...TOAST_DEFAULTS, ...options })
}

/** 信息提示（蓝色） */
export function toastInfo(message: string, options?: ExternalToast) {
  return toast.info(message, { ...TOAST_DEFAULTS, ...options })
}

/** 加载中提示（旋转图标），需手动 toast.dismiss(id) 结束 */
export function toastLoading(message: string, options?: ExternalToast) {
  return toast.loading(message, { ...TOAST_DEFAULTS, ...options })
}

// 透传 sonner 原始 toast，供自定义场景使用（toast.promise / toast.dismiss 等）
export { toast }
