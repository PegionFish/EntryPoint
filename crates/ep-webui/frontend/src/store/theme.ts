import { create } from 'zustand'
import { fetchServerConfig, putConfigPatch } from '@/hooks/use-config'

export type Theme = 'dark' | 'light'

const STORAGE_KEY = 'ep-theme'

interface ThemeState {
  theme: Theme
  /** 本地立即应用（DOM + localStorage）并即时回写服务器（P2-2 三端同步） */
  setTheme: (theme: Theme) => void
  /** 切换主题（等价于 setTheme(另一个主题)） */
  toggle: () => void
}

function isTheme(v: unknown): v is Theme {
  return v === 'dark' || v === 'light'
}

function applyTheme(theme: Theme): void {
  document.documentElement.classList.toggle('dark', theme === 'dark')
  try {
    localStorage.setItem(STORAGE_KEY, theme)
  } catch {
    // 忽略存储失败（如隐私模式）
  }
}

function initialTheme(): Theme {
  try {
    const stored = localStorage.getItem(STORAGE_KEY)
    if (stored === 'dark' || stored === 'light') return stored
  } catch {
    // ignore
  }
  return 'dark' // 默认深色主题
}

/**
 * 主题回写服务器：PUT /api/config 合并单键 `{general:{theme}}`（§8.2）。
 * 本地已先行应用，回写为 best-effort —— 失败仅告警，不打断交互；
 * 服务器仍为权威真源，下次启动同步时对齐（P2-2）。
 *
 * 300ms 去抖合并：顶栏 toggle 与设置页 Select 都会触发持久化，
 * 快速连续切换时合并为最后一次，避免并发 PUT 乱序导致服务器最终值回退（P2）。
 */
const PERSIST_DEBOUNCE_MS = 300
let persistTimer: number | null = null

function persistTheme(theme: Theme): void {
  if (persistTimer !== null) window.clearTimeout(persistTimer)
  persistTimer = window.setTimeout(() => {
    persistTimer = null
    void putConfigPatch({ general: { theme } }).catch((e: unknown) => {
      console.warn('[theme] 回写服务器失败（本地已应用）:', e)
    })
  }, PERSIST_DEBOUNCE_MS)
}

/**
 * 采纳指定主题为当前状态，但**不回写服务器**。
 * 供「服务器权威」同步场景使用（启动拉取 / 设置页重载后的对齐），
 * 避免把服务器真源值再写回去造成无谓写入。
 */
export function adoptTheme(theme: Theme): void {
  applyTheme(theme)
  useThemeStore.setState({ theme })
}

/**
 * 启动同步：读取服务器 theme（P2-2 修「启动不读服务器」）。
 * 本地 localStorage 主题已在模块加载时先行应用（防首屏闪烁），
 * 服务器值有效且不一致时以服务器为准（冲突时服务器优先）。
 */
export async function syncThemeFromServer(): Promise<void> {
  try {
    const cfg = await fetchServerConfig()
    const serverTheme = cfg.general?.theme
    if (isTheme(serverTheme) && serverTheme !== useThemeStore.getState().theme) {
      adoptTheme(serverTheme)
    }
  } catch (e) {
    console.warn('[theme] 读取服务器主题失败，保留本地主题:', e)
  }
}

export const useThemeStore = create<ThemeState>((set) => ({
  theme: initialTheme(),
  setTheme: (theme) => {
    applyTheme(theme)
    set({ theme })
    persistTheme(theme)
  },
  toggle: () => {
    const theme: Theme =
      useThemeStore.getState().theme === 'dark' ? 'light' : 'dark'
    applyTheme(theme)
    set({ theme })
    persistTheme(theme)
  },
}))

// 模块加载时立即应用本地主题避免首屏闪烁；随后异步与服务器对齐（服务器优先）。
// 桌面端主题体系独立（egui 自有主题），本文件仅服务 WebUI 三端同步。
applyTheme(initialTheme())
void syncThemeFromServer()
