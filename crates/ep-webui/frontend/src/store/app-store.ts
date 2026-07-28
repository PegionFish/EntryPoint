import { create } from 'zustand'
import { useThemeStore, type Theme } from '@/store/theme'

export interface AppState {
  /** 当前主题，与 useThemeStore 双向同步（theme store 负责 DOM 应用与持久化） */
  theme: Theme
  /** 全局 WebSocket 是否已连接 */
  wsConnected: boolean
  /** 全局加载态（路由级 / 批量操作期间） */
  globalLoading: boolean
  setTheme: (theme: Theme) => void
  setWsConnected: (connected: boolean) => void
  setGlobalLoading: (loading: boolean) => void
}

export const useAppStore = create<AppState>((set) => ({
  theme: useThemeStore.getState().theme,
  wsConnected: false,
  globalLoading: false,

  setTheme: (theme) => {
    // 委托给 theme store：由它切换 <html> 的 dark 类并持久化到 localStorage
    useThemeStore.getState().setTheme(theme)
    set({ theme })
  },
  setWsConnected: (wsConnected) => set({ wsConnected }),
  setGlobalLoading: (globalLoading) => set({ globalLoading }),
}))

// 反向同步：theme store 被直接使用（如 toggle()）时，保持 app store 一致
useThemeStore.subscribe((state) => {
  if (useAppStore.getState().theme !== state.theme) {
    useAppStore.setState({ theme: state.theme })
  }
})
