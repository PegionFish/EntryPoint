import { clsx, type ClassValue } from "clsx"
import { twMerge } from "tailwind-merge"

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

/** 将秒数格式化为运行时长，例如 3661 → "1h 1m 1s" */
export function formatUptime(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return "-"
  const s = Math.floor(secs)
  const d = Math.floor(s / 86400)
  const h = Math.floor((s % 86400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  if (d > 0) return `${d}d ${h}h ${m}m`
  if (h > 0) return `${h}h ${m}m ${sec}s`
  if (m > 0) return `${m}m ${sec}s`
  return `${sec}s`
}

/** 将字节数格式化为可读大小，例如 1536 → "1.5 KB" */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return "-"
  if (bytes === 0) return "0 B"
  const units = ["B", "KB", "MB", "GB", "TB"]
  const i = Math.min(
    units.length - 1,
    Math.floor(Math.log(Math.abs(bytes)) / Math.log(1024)),
  )
  const value = bytes / Math.pow(1024, i)
  return `${value >= 100 ? Math.round(value) : value.toFixed(1)} ${units[i]}`
}

/** 将 MB 数值格式化为可读大小，例如 2048 → "2.0 GB" */
export function formatMB(mb: number | null | undefined): string {
  if (mb === null || mb === undefined || !Number.isFinite(mb)) return "-"
  return formatBytes(mb * 1024 * 1024)
}
