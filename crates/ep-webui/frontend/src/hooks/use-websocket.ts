import { useEffect, useRef, useState } from 'react'
import { WebSocketManager, wsManager, type WsConnectionState } from '@/api/ws'

/** 与 App.tsx 共享的全局连接路径；命中时复用 wsManager 单例 */
const GLOBAL_WS_PATH = '/ws'

export interface UseWebSocketResult {
  /** 已建立连接 */
  connected: boolean
  /** 首次连接中或断线重连中 */
  connecting: boolean
  /** 已断开（尚未连接 / 不再重连） */
  disconnected: boolean
}

function resolveUrl(path: string): string {
  const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  const normalized = path.startsWith('/') ? path : `/${path}`
  return `${proto}//${window.location.host}${normalized}`
}

/**
 * 订阅 WebSocket 的通用 hook，封装 @/api/ws 的 WebSocketManager。
 *
 * - `path` 为全局路径 `/ws` 时复用 `wsManager` 单例（连接生命周期由 App.tsx 管理），
 *   否则创建该 hook 专属的连接，并在卸载时断开。
 * - 自动重连（指数退避 1s → … → 30s）由 WebSocketManager 内部实现。
 * - 页面不可见时暂停专属连接的重连；页面恢复可见时立即重连，
 *   不等待剩余退避时间（对全局连接仅做"可见即重连"，不主动断开）。
 * - `onMessage` 通过 ref 持有最新引用，回调变化不会导致重新订阅。
 */
export function useWebSocket<T>(
  path: string,
  onMessage: (data: T) => void,
): UseWebSocketResult {
  const [state, setState] = useState<WsConnectionState>('idle')
  const onMessageRef = useRef(onMessage)
  onMessageRef.current = onMessage

  useEffect(() => {
    const isGlobal = path === GLOBAL_WS_PATH
    const manager = isGlobal ? wsManager : new WebSocketManager(resolveUrl(path))

    const unsubscribeMessage = manager.onMessage((msg) => {
      onMessageRef.current(msg as T)
    })
    // onStateChange 订阅时会立即推送一次当前状态
    const unsubscribeState = manager.onStateChange(setState)

    if (!isGlobal) {
      manager.connect()
    }

    const handleVisibility = () => {
      if (document.hidden) {
        // 页面隐藏：暂停重连（仅断开 hook 专属连接；全局连接交由 App 管理）
        if (!isGlobal) manager.disconnect()
      } else {
        // 页面恢复可见：若未连接则立即重连，跳过剩余退避等待
        const current = manager.getState()
        if (current !== 'connected' && current !== 'connecting') {
          manager.connect()
        }
      }
    }
    document.addEventListener('visibilitychange', handleVisibility)

    return () => {
      document.removeEventListener('visibilitychange', handleVisibility)
      unsubscribeMessage()
      unsubscribeState()
      if (!isGlobal) manager.disconnect()
    }
  }, [path])

  return {
    connected: state === 'connected',
    connecting: state === 'connecting' || state === 'reconnecting',
    disconnected: state === 'disconnected' || state === 'idle',
  }
}
