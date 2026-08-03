import type { WsMessage } from './types'

/** WebSocket 连接状态 */
export type WsConnectionState =
  | 'idle' // 尚未连接
  | 'connecting' // 首次连接中
  | 'connected' // 已连接
  | 'reconnecting' // 断线后重连中
  | 'disconnected' // 已断开（不再重连）

export type { WsMessage } from './types'

type MessageListener = (msg: WsMessage) => void
type StateListener = (state: WsConnectionState) => void

const MIN_BACKOFF_MS = 1000
const MAX_BACKOFF_MS = 30000

/**
 * WebSocket 连接管理器。
 *
 * - 自动重连：指数退避 1s → 2s → 4s → 8s → … → 上限 30s，连接成功后重置。
 * - 状态跟踪：通过 onStateChange 订阅连接状态变化。
 * - 消息解析：自动 JSON.parse 后分发给 onMessage 订阅者。
 */
export class WebSocketManager {
  private url: string
  private ws: WebSocket | null = null
  private state: WsConnectionState = 'idle'
  private messageListeners = new Set<MessageListener>()
  private stateListeners = new Set<StateListener>()
  private reconnectAttempt = 0
  private reconnectTimer: number | null = null
  private shouldReconnect = true

  constructor(url?: string) {
    this.url = url ?? WebSocketManager.defaultUrl()
  }

  private static defaultUrl(): string {
    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    return `${proto}//${window.location.host}/ws`
  }

  /** 建立连接（幂等：已连接/连接中时直接返回） */
  connect(): void {
    if (this.ws && (this.state === 'connected' || this.state === 'connecting')) {
      return
    }
    this.shouldReconnect = true
    this.openSocket(this.state === 'idle' ? 'connecting' : 'reconnecting')
  }

  /** 主动断开并停止重连 */
  disconnect(): void {
    this.shouldReconnect = false
    this.clearReconnectTimer()
    if (this.ws) {
      this.ws.onclose = null
      this.ws.onerror = null
      this.ws.onmessage = null
      this.ws.close()
      this.ws = null
    }
    this.setState('disconnected')
  }

  getState(): WsConnectionState {
    return this.state
  }

  /** 订阅消息，返回取消订阅函数 */
  onMessage(fn: MessageListener): () => void {
    this.messageListeners.add(fn)
    return () => this.messageListeners.delete(fn)
  }

  /** 订阅连接状态变化，返回取消订阅函数 */
  onStateChange(fn: StateListener): () => void {
    this.stateListeners.add(fn)
    // 立即推送一次当前状态，方便订阅方初始化 UI
    fn(this.state)
    return () => this.stateListeners.delete(fn)
  }

  /** 发送消息（仅在已连接时有效） */
  send(data: unknown): boolean {
    if (!this.ws || this.state !== 'connected') return false
    this.ws.send(typeof data === 'string' ? data : JSON.stringify(data))
    return true
  }

  private openSocket(initialState: WsConnectionState): void {
    this.setState(initialState)
    let socket: WebSocket
    try {
      socket = new WebSocket(this.url)
    } catch {
      this.scheduleReconnect()
      return
    }
    this.ws = socket

    socket.onopen = () => {
      this.reconnectAttempt = 0
      this.setState('connected')
    }

    socket.onmessage = (event) => {
      const parsed = this.parse(event.data)
      if (parsed === undefined) return
      this.messageListeners.forEach((fn) => {
        try {
          fn(parsed as WsMessage)
        } catch {
          // 单个订阅者抛错不应影响其他订阅者
        }
      })
    }

    socket.onclose = () => {
      this.ws = null
      if (this.shouldReconnect) {
        this.scheduleReconnect()
      } else {
        this.setState('disconnected')
      }
    }

    socket.onerror = () => {
      // 错误后通常会触发 onclose，由 onclose 负责重连
      socket.close()
    }
  }

  private parse(data: unknown): unknown {
    if (typeof data !== 'string') return data
    try {
      return JSON.parse(data)
    } catch {
      return undefined
    }
  }

  private scheduleReconnect(): void {
    this.setState('reconnecting')
    const delay = Math.min(
      MIN_BACKOFF_MS * Math.pow(2, this.reconnectAttempt),
      MAX_BACKOFF_MS,
    )
    this.reconnectAttempt += 1
    this.clearReconnectTimer()
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null
      this.openSocket('reconnecting')
    }, delay)
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      window.clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
  }

  private setState(next: WsConnectionState): void {
    if (this.state === next) return
    this.state = next
    this.stateListeners.forEach((fn) => {
      try {
        fn(next)
      } catch {
        // ignore
      }
    })
  }
}

/** 全局单例，供整个应用共享同一条 WS 连接 */
export const wsManager = new WebSocketManager()
