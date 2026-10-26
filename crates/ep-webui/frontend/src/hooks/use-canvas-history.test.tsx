import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import {
  HISTORY_LIMIT,
  useCanvasHistory,
  type CanvasSnapshot,
  type UseCanvasHistoryResult,
} from './use-canvas-history'

/**
 * T-28b spike：命令栈 ↔ xyflow 受控状态同步链路验证。
 *
 * xyflow 受控态在测试里以 `{ nodes, edges }` 数组模拟（useNodesState/
 * useEdgesState 本质是 useState + 不可变更新，快照引用语义一致）。
 * 核心链路：框选 2 节点 → 批量删除（一次 commit）→ 一步 undo 全量恢复。
 */

interface FakeNode {
  id: string
  x: number
}

const n = (id: string, x = 0): FakeNode => ({ id, x })

type History = UseCanvasHistoryResult<FakeNode, unknown>
type Snapshot = CanvasSnapshot<FakeNode, unknown>

/** 模拟受控画布：驱动 hook 的最小 harness（hook 调用统一经 act 触发重渲染） */
function makeCanvas(hook: { result: { current: History } }) {
  let nodes: FakeNode[] = [n('a'), n('b'), n('c')]
  let edges: unknown[] = []
  const current = () => hook.result.current

  /** 应用快照到受控态（模拟 setNodes/setEdges） */
  function apply(snapshot: Snapshot | null) {
    if (snapshot) {
      nodes = snapshot.nodes
      edges = snapshot.edges
    }
  }

  return {
    get nodes() {
      return nodes
    },
    deleteNodes(ids: string[]) {
      act(() => {
        const before = current().beginChange(nodes, edges)
        nodes = nodes.filter((node) => !ids.includes(node.id))
        current().commitChange(before, { nodes, edges }, 'delete')
      })
    },
    moveNode(id: string, x: number, key = `move:${id}`) {
      act(() => {
        const before = current().beginChange(nodes, edges)
        nodes = nodes.map((node) => (node.id === id ? { ...node, x } : node))
        current().commitChange(before, { nodes, edges }, 'move', { coalesceKey: key })
      })
    },
    undo(): Snapshot | null {
      let snap: Snapshot | null = null
      act(() => {
        snap = current().undo()
      })
      apply(snap)
      return snap
    },
    redo(): Snapshot | null {
      let snap: Snapshot | null = null
      act(() => {
        snap = current().redo()
      })
      apply(snap)
      return snap
    },
  }
}

describe('useCanvasHistory（T-28b 命令栈 spike）', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('spike 链路：批量删除 2 节点 = 一步 undo 全量恢复', () => {
    const { result } = renderHook(() => useCanvasHistory<FakeNode, unknown>())
    const canvas = makeCanvas({ result })

    expect(result.current.canUndo).toBe(false)

    // 框选 b/c → 批量删除（单次 commit）
    canvas.deleteNodes(['b', 'c'])
    expect(canvas.nodes.map((node) => node.id)).toEqual(['a'])
    expect(result.current.canUndo).toBe(true)
    expect(result.current.canRedo).toBe(false)

    // 一步 undo：b/c 全量恢复
    canvas.undo()
    expect(canvas.nodes.map((node) => node.id)).toEqual(['a', 'b', 'c'])
    expect(result.current.canUndo).toBe(false)
    expect(result.current.canRedo).toBe(true)

    // redo 回到删除后
    canvas.redo()
    expect(canvas.nodes.map((node) => node.id)).toEqual(['a'])
  })

  it('undo 后新提交丢弃 redo 分支', () => {
    const { result } = renderHook(() => useCanvasHistory<FakeNode, unknown>())
    const canvas = makeCanvas({ result })

    canvas.deleteNodes(['b'])
    canvas.deleteNodes(['c'])
    expect(canvas.nodes.map((node) => node.id)).toEqual(['a'])
    canvas.undo()
    expect(canvas.nodes.map((node) => node.id)).toEqual(['a', 'c'])

    canvas.deleteNodes(['c']) // 新操作覆盖 redo 分支
    expect(result.current.canRedo).toBe(false)
    canvas.undo()
    expect(canvas.nodes.map((node) => node.id)).toEqual(['a', 'c'])
  })

  it('coalesceKey 时间窗内合并为一步（连续输入不刷屏）', () => {
    const { result } = renderHook(() => useCanvasHistory<FakeNode, unknown>())
    const canvas = makeCanvas({ result })

    canvas.moveNode('a', 10)
    canvas.moveNode('a', 20)
    canvas.moveNode('a', 30)
    // 三次拖动/输入合并为一步
    canvas.undo()
    expect(canvas.nodes.find((node) => node.id === 'a')?.x).toBe(0)
    expect(result.current.canUndo).toBe(false)

    // 超出合并窗口（700ms）后成为独立一步
    canvas.moveNode('a', 10)
    vi.advanceTimersByTime(800)
    canvas.moveNode('a', 20)
    canvas.undo()
    expect(canvas.nodes.find((node) => node.id === 'a')?.x).toBe(10)
    canvas.undo()
    expect(canvas.nodes.find((node) => node.id === 'a')?.x).toBe(0)
  })

  it('不同 coalesceKey 不合并', () => {
    const { result } = renderHook(() => useCanvasHistory<FakeNode, unknown>())
    const canvas = makeCanvas({ result })

    canvas.moveNode('a', 10)
    canvas.moveNode('b', 10, 'move:b')
    canvas.undo()
    expect(canvas.nodes.find((node) => node.id === 'b')?.x).toBe(0)
    expect(canvas.nodes.find((node) => node.id === 'a')?.x).toBe(10)
  })

  it(`栈深上限 ${HISTORY_LIMIT}：超限丢最旧`, () => {
    const { result } = renderHook(() => useCanvasHistory<FakeNode, unknown>())
    const canvas = makeCanvas({ result })

    for (let i = 1; i <= HISTORY_LIMIT + 20; i += 1) {
      vi.advanceTimersByTime(1000) // 避免合并
      canvas.moveNode('a', i)
    }
    // 连续 undo 到底：最早可恢复到 x = 20（前 20 步已被丢弃）
    let count = 0
    while (result.current.canUndo) {
      canvas.undo()
      count += 1
    }
    expect(count).toBe(HISTORY_LIMIT)
    expect(canvas.nodes.find((node) => node.id === 'a')?.x).toBe(20)
  })

  it('空栈 undo / 栈顶 redo 返回 null；clear 清空', () => {
    const { result } = renderHook(() => useCanvasHistory<FakeNode, unknown>())
    const canvas = makeCanvas({ result })

    expect(result.current.undo()).toBeNull()
    expect(result.current.redo()).toBeNull()

    canvas.deleteNodes(['b'])
    act(() => result.current.clear())
    expect(result.current.canUndo).toBe(false)
    expect(result.current.undo()).toBeNull()
  })
})
