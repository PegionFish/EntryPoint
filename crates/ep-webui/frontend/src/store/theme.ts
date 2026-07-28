import { create } from 'zustand'

export type Theme = 'dark' | 'light'

const STORAGE_KEY = 'ep-theme'

interface ThemeState {
  theme: Theme
  setTheme: (theme: Theme) => void
  toggle: () => void
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

export const useThemeStore = create<ThemeState>((set) => ({
  theme: initialTheme(),
  setTheme: (theme) => {
    applyTheme(theme)
    set({ theme })
  },
  toggle: () =>
    set((state) => {
      const theme: Theme = state.theme === 'dark' ? 'light' : 'dark'
      applyTheme(theme)
      return { theme }
    }),
}))

// 模块加载时立即应用初始主题，避免首屏闪烁
applyTheme(initialTheme())
