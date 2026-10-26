import type { Edge } from '@xyflow/react'

import { getParamSpecs } from '@/components/shared/pipeline-node'
import type {
  NodeParams,
  ParamSpec,
  ParamValue,
  PipelineFlowNode,
} from '@/components/shared/pipeline-node'

/**
 * T-28b 多模块编辑：选区批量操作的纯函数集。
 * 与 xyflow 解耦（输入画布节点/边数组，输出新数组），可直测。
 */

/** 方向键步进距离（画布点阵 gap=22，取整 20 视觉对齐） */
export const NUDGE_STEP = 20

/** 克隆节点相对原节点的偏移（避免完全重叠看不出新副本） */
export const CLONE_OFFSET = { x: 32, y: 32 }

/** 克隆节点 id 前缀（与 createPipelineNode 的 uid 形态一致） */
function shortUuid(): string {
  return crypto.randomUUID().slice(0, 8)
}

// ============================================================
// 对齐 / 均布
// ============================================================

export type AlignMode = 'left' | 'top' | 'hdist' | 'vdist'

/**
 * 对齐/均布选区节点，返回更新位置后的新节点数组（未选节点原引用返回）。
 * - left/top：统一到选区包围盒左缘/上缘；
 * - hdist/vdist：按 x/y 排序后首尾不动、中间等距（<3 个节点为无操作）。
 */
export function alignSelected(
  nodes: PipelineFlowNode[],
  ids: readonly string[],
  mode: AlignMode,
): PipelineFlowNode[] {
  const idSet = new Set(ids)
  const selected = nodes.filter((n) => idSet.has(n.id))
  if (selected.length < 2) return nodes

  /** 目标坐标映射（节点 id → 对齐/均布后的 x 或 y） */
  const coordById = new Map<string, number>()
  if (mode === 'left') {
    const minX = Math.min(...selected.map((n) => n.position.x))
    for (const n of selected) coordById.set(n.id, minX)
  } else if (mode === 'top') {
    const minY = Math.min(...selected.map((n) => n.position.y))
    for (const n of selected) coordById.set(n.id, minY)
  } else {
    const axis = mode === 'hdist' ? 'x' : 'y'
    const sorted = [...selected].sort((a, b) => a.position[axis] - b.position[axis])
    const first = sorted[0]!
    const last = sorted[sorted.length - 1]!
    const step = (last.position[axis] - first.position[axis]) / (sorted.length - 1)
    sorted.forEach((n, i) => coordById.set(n.id, first.position[axis] + i * step))
  }

  return nodes.map((n) => {
    const coord = coordById.get(n.id)
    if (coord === undefined) return n
    return {
      ...n,
      position:
        mode === 'top' || mode === 'vdist'
          ? { ...n.position, y: coord }
          : { ...n.position, x: coord },
    }
  })
}

// ============================================================
// 方向键步进
// ============================================================

/** 选区节点整体位移（方向键步进/网格对齐移动），返回新节点数组 */
export function nudgeSelected(
  nodes: PipelineFlowNode[],
  ids: readonly string[],
  dx: number,
  dy: number,
): PipelineFlowNode[] {
  const idSet = new Set(ids)
  if (idSet.size === 0 || (dx === 0 && dy === 0)) return nodes
  return nodes.map((n) => {
    if (!idSet.has(n.id)) return n
    return {
      ...n,
      position: { x: n.position.x + dx, y: n.position.y + dy },
    }
  })
}

// ============================================================
// 克隆（复制节点 + 选区内部边，新 id）
// ============================================================

export interface CloneResult {
  /** 克隆出的新节点（追加到画布用） */
  nodes: PipelineFlowNode[]
  /** 选区内部边的克隆（端点已重映射到新节点 id） */
  edges: Edge[]
}

/**
 * 克隆选区：节点新 id、位置偏移 CLONE_OFFSET；仅克隆两端都在选区内的边
 * （跨选区边不克隆，避免语义歧义）；克隆节点不继承选中态与运行状态。
 */
export function cloneSelection(
  nodes: PipelineFlowNode[],
  edges: Edge[],
  ids: readonly string[],
  makeId: () => string = shortUuid,
  offset = CLONE_OFFSET,
): CloneResult {
  const idSet = new Set(ids)
  const selected = nodes.filter((n) => idSet.has(n.id))
  if (selected.length === 0) return { nodes: [], edges: [] }

  const idMap = new Map<string, string>(selected.map((n) => [n.id, makeId()]))
  const clonedNodes: PipelineFlowNode[] = selected.map((n) => ({
    ...n,
    id: idMap.get(n.id)!,
    selected: false,
    position: { x: n.position.x + offset.x, y: n.position.y + offset.y },
    data: {
      ...n.data,
      status: 'waiting' as const,
      params: { ...n.data.params } as NodeParams,
    },
  }) as PipelineFlowNode)

  const clonedEdges: Edge[] = edges
    .filter((e) => idSet.has(e.source) && idSet.has(e.target))
    .map((e) => ({
      ...e,
      id: `e-${idMap.get(e.source)!}->${idMap.get(e.target)!}`,
      source: idMap.get(e.source)!,
      target: idMap.get(e.target)!,
      selected: false,
    }))

  return { nodes: clonedNodes, edges: clonedEdges }
}

// ============================================================
// 批量参数编辑（多选同类型节点的公共参数面）
// ============================================================

export interface ParamShape {
  /** 形状键（同 key 才视为同类型：builtin:<名> / module:<模块id>:<能力id>） */
  key: string
  /** 该形状的参数表（getParamSpecs） */
  specs: ParamSpec[]
  /** 展示名（节点 label 或能力名） */
  title: string
}

/**
 * 选区公共参数形状：全部选中节点为同一 builtin、或同模块同能力时返回
 * 参数表；否则返回 null（不展示批量参数面）。
 */
export function commonParamShape(nodes: PipelineFlowNode[], ids: readonly string[]): ParamShape | null {
  if (ids.length < 2) return null
  const idSet = new Set(ids)
  const selected = nodes.filter((n) => idSet.has(n.id))
  if (selected.length !== ids.length) return null

  const shapes = new Set<string>()
  for (const n of selected) {
    if (n.data.kind === 'builtin') shapes.add(`builtin:${n.data.builtin}`)
    else if (n.data.kind === 'module')
      shapes.add(`module:${n.data.moduleId}:${n.data.capabilityId}`)
    else return null // external 不参与批量参数编辑
  }
  if (shapes.size !== 1) return null

  const first = selected[0]!
  return {
    key: [...shapes][0]!,
    specs: getParamSpecs(first.data),
    title: first.data.label,
  }
}

/** 参数值规范化比较（区分 0/false 与缺省；数组按序） */
function paramValueKey(v: ParamValue | undefined): string {
  if (v === undefined) return '<undefined>'
  return JSON.stringify(v)
}

/** 单个参数字段在选区内的取值状态 */
export interface ParamFieldState {
  spec: ParamSpec
  /** 首个选中节点的当前值（面板渲染值） */
  value: ParamValue | undefined
  /** 选区内取值不一致（面板显示「混合」标记） */
  mixed: boolean
}

/** 计算选区各参数字段的取值状态（逐字段「已改/混合」标记数据源） */
export function paramFieldStates(
  nodes: PipelineFlowNode[],
  ids: readonly string[],
  specs: readonly ParamSpec[],
): ParamFieldState[] {
  const idSet = new Set(ids)
  const selected = nodes.filter((n) => idSet.has(n.id))
  return specs.map((spec) => {
    const values = selected.map((n) => paramValueKey(n.data.params[spec.name]))
    return {
      spec,
      value: selected[0]?.data.params[spec.name],
      mixed: new Set(values).size > 1,
    }
  })
}
