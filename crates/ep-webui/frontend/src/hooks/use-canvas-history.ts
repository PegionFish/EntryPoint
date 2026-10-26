import { useCallback, useMemo, useRef, useState } from 'react'

/**
 * T-28b：管线画布撤销/重做命令栈（spike 结论落地）。
 *
 * ## 单一真源方案（spike 选型）
 *
 * 画布状态仍是 xyflow 的受控 `useNodesState`/`useEdgesState` 两个 useState——
 * 命令栈**不接管**状态，只保存语义操作前后的**快照引用对**（shallow 数组拷贝，
 * 节点/边对象按引用共享）。理由：
 *
 * 1. xyflow 的 `applyNodeChanges` 走不可变更新（拖动/删除都产生新对象），
 *    快照引用恒有效——undo 恢复的是原对象（含 `measured` 尺寸缓存），
 *    不触发重新测量闪烁；
 * 2. 批量操作天然一步：一次 commit 内任意多次 setNodes/setEdges，
 *    before/after 各一份，批量删除 = 一步 undo；
 * 3. 选择态（xyflow 内部 `selected` 标志）不入栈：undo/redo 应用快照时由
 *    调用方剥离 `selected` 并清理页面层选中态（undo 后选中态清理）。
 *
 * 语义操作（提交点）= 节点增删 / 拖动（dragStop 一次）/ 参数修改 /
 * 连线变更 / 对齐 / 克隆；连续编辑（输入框逐键、方向键步进）用
 * `coalesceKey` 在时间窗内合并为一步。加载/新建/导入整画布替换走
 * `clear()`（历史断层）。栈深上限 `HISTORY_LIMIT`，超限丢弃最旧条目。
 */

/** 画布快照（节点/边数组浅拷贝；对象按引用共享） */
export interface CanvasSnapshot<N, E> {
  nodes: N[]
  edges: E[]
}

interface HistoryEntry<N, E> {
  label: string
  before: CanvasSnapshot<N, E>
  after: CanvasSnapshot<N, E>
  /** 非空时与同 key 且时间窗内的上一步合并（连续输入/步进） */
  coalesceKey?: string
  /** 该步最后写入时间（合并窗口判定） */
  at: number
}

/** 栈深上限 */
export const HISTORY_LIMIT = 100

/** 连续编辑合并窗口（毫秒）：一次输入停顿后即视为新一步 */
const COALESCE_WINDOW_MS = 700

export interface UseCanvasHistoryResult<N, E> {
  /** 是否可撤销 */
  canUndo: boolean
  /** 是否可重做 */
  canRedo: boolean
  /** 变更前调用：以当前画布状态捕获 before 快照 */
  beginChange: (nodes: N[], edges: E[]) => CanvasSnapshot<N, E>
  /**
   * 变更后调用：以变更后状态捕获 after 并入栈一步。
   * @param opts.coalesceKey 同 key 且 COALESCE_WINDOW_MS 内的推送合并为一步
   *   （连续输入/方向键步进不刷屏栈）
   */
  commitChange: (
    before: CanvasSnapshot<N, E>,
    after: CanvasSnapshot<N, E>,
    label: string,
    opts?: { coalesceKey?: string },
  ) => void
  /** 撤销：返回应恢复的快照；无可撤销返回 null（调用方负责应用并清理选中态） */
  undo: () => CanvasSnapshot<N, E> | null
  /** 重做：返回应恢复的快照；无可重做返回 null */
  redo: () => CanvasSnapshot<N, E> | null
  /** 清空历史（整画布替换：加载/新建/导入时调用） */
  clear: () => void
}

export function useCanvasHistory<N, E>(): UseCanvasHistoryResult<N, E> {
  // 条目存 ref（快照引用对，无深拷贝）；已应用条目数同时持有
  // ref（同 tick 连续调用读最新值）与 state（驱动 canUndo/canRedo 重渲染）
  const entriesRef = useRef<HistoryEntry<N, E>[]>([])
  const indexRef = useRef(0) // 已应用条目数（undo/redo 指针，逻辑读）
  const [index, setIndex] = useState(0) // 同上（渲染读）

  const beginChange = useCallback(
    (nodes: N[], edges: E[]): CanvasSnapshot<N, E> => ({
      nodes: [...nodes],
      edges: [...edges],
    }),
    [],
  )

  /** 同步推进指针（ref 供同 tick 连续调用读最新值）+ 触发渲染 */
  const advanceTo = useCallback((nextIndex: number) => {
    indexRef.current = nextIndex
    setIndex(nextIndex)
  }, [])

  const commitChange = useCallback(
    (
      before: CanvasSnapshot<N, E>,
      after: CanvasSnapshot<N, E>,
      label: string,
      opts?: { coalesceKey?: string },
    ): void => {
      const entries = entriesRef.current
      const index = indexRef.current
      const last = index > 0 ? entries[index - 1] : undefined
      const now = Date.now()
      if (
        last &&
        opts?.coalesceKey &&
        last.coalesceKey === opts.coalesceKey &&
        now - last.at <= COALESCE_WINDOW_MS &&
        index === entries.length
      ) {
        // 连续编辑合并：只更新上一步的 after（before 保持最初态；指针不动）。
        // 收官审查 P1-1：必须指针在栈顶才允许合并——undo 后指针下方存在旧
        // 条目，同窗重做同 key 操作若写入 last.after 会污染历史（redo 复活
        // 过期分支并丢弃用户的新步进）。
        last.after = after
        last.at = now
        return
      }
      // 丢弃 redo 分支后入栈；超限丢最旧
      const next = entries.slice(0, index)
      next.push({ label, before, after, coalesceKey: opts?.coalesceKey, at: now })
      if (next.length > HISTORY_LIMIT) next.shift()
      entriesRef.current = next
      advanceTo(next.length)
    },
    [advanceTo],
  )

  const undo = useCallback((): CanvasSnapshot<N, E> | null => {
    const entries = entriesRef.current
    const index = indexRef.current
    if (index === 0) return null
    const entry = entries[index - 1]
    advanceTo(index - 1)
    return entry.before
  }, [advanceTo])

  const redo = useCallback((): CanvasSnapshot<N, E> | null => {
    const entries = entriesRef.current
    const index = indexRef.current
    if (index >= entries.length) return null
    const entry = entries[index]
    advanceTo(index + 1)
    return entry.after
  }, [advanceTo])

  const clear = useCallback(() => {
    entriesRef.current = []
    advanceTo(0)
  }, [advanceTo])

  // index 为渲染态：每次 push/undo/redo 后 canUndo/canRedo 同步刷新且返回引用
  // 稳定（消费方 useCallback/Effect 不必每次渲染重建）。栈长读 entriesRef——
  // 变更必然伴随 setIndex，渲染值恒新鲜。
  return useMemo(
    () => ({
      canUndo: index > 0,
      canRedo: index < entriesRef.current.length,
      beginChange,
      commitChange,
      undo,
      redo,
      clear,
    }),
    [index, beginChange, commitChange, undo, redo, clear],
  )
}
