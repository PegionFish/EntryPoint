import { useEffect, useState } from 'react'
import { wsManager, type WsConnectionState } from '@/api/ws'

/** 订阅全局 WebSocket 连接状态 */
export function useWsState(): WsConnectionState {
  const [state, setState] = useState<WsConnectionState>(wsManager.getState())
  useEffect(() => wsManager.onStateChange(setState), [])
  return state
}
