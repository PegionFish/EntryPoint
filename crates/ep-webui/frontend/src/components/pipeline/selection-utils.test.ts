import type { Edge } from '@xyflow/react'
import { describe, expect, it } from 'vitest'

import type { ModuleFlowNode, NodeParams, PipelineFlowNode } from '@/components/shared/pipeline-node'
import {
  alignSelected,
  CLONE_OFFSET,
  cloneSelection,
  commonParamShape,
  nudgeSelected,
  NUDGE_STEP,
  paramFieldStates,
} from './selection-utils'

/** 最小 builtin 画布节点 */
function bn(id: string, x: number, y: number, params: NodeParams = {}): PipelineFlowNode {
  return {
    id,
    type: 'builtin',
    position: { x, y },
    data: {
      kind: 'builtin',
      builtin: 'ffmpeg',
      label: id,
      status: 'waiting',
      params,
    },
  }
}

function mn(id: string, x: number, y: number, params: NodeParams = {}): PipelineFlowNode {
  return {
    id,
    type: 'module',
    position: { x, y },
    data: {
      kind: 'module',
      label: id,
      moduleId: 'deep-filter',
      moduleVersion: '0.5.6',
      category: 'denoise',
      capabilityId: 'denoise',
      capabilityLabel: 'denoise',
      status: 'waiting',
      params,
    },
  } as ModuleFlowNode as PipelineFlowNode
}

const e = (source: string, target: string): Edge => ({
  id: `e-${source}->${target}`,
  source,
  target,
  sourceHandle: 'out',
  targetHandle: 'in',
})

describe('selection-utils（T-28b 批量操作纯函数）', () => {
  it('alignSelected left/top：统一到选区包围盒左缘/上缘', () => {
    const nodes = [bn('a', 100, 50), bn('b', 40, 90), bn('c', 220, 10)]
    const left = alignSelected(nodes, ['a', 'b', 'c'], 'left')
    expect(left.map((n) => n.position.x)).toEqual([40, 40, 40])
    expect(left.map((n) => n.position.y)).toEqual([50, 90, 10])

    const top = alignSelected(nodes, ['a', 'b', 'c'], 'top')
    expect(top.map((n) => n.position.y)).toEqual([10, 10, 10])
  })

  it('alignSelected hdist/vdist：首尾不动、中间等距', () => {
    const nodes = [bn('a', 0, 0), bn('b', 90, 0), bn('c', 200, 0)]
    const dist = alignSelected(nodes, ['a', 'b', 'c'], 'hdist')
    expect(dist.map((n) => n.position.x)).toEqual([0, 100, 200])

    // 单节点/双节点均布为无操作（原引用返回）
    expect(alignSelected(nodes, ['a'], 'hdist')).toBe(nodes)
    expect(alignSelected(nodes, ['a', 'b'], 'vdist').find((n) => n.id === 'b')?.position.y).toBe(0)
  })

  it('nudgeSelected：仅移动选区节点，步进常量可整除对齐', () => {
    const nodes = [bn('a', 100, 100), bn('b', 0, 0)]
    const moved = nudgeSelected(nodes, ['a'], NUDGE_STEP, 0)
    expect(moved.find((n) => n.id === 'a')?.position).toEqual({ x: 100 + NUDGE_STEP, y: 100 })
    expect(moved.find((n) => n.id === 'b')?.position).toEqual({ x: 0, y: 0 })
    expect(nudgeSelected(nodes, [], 5, 5)).toBe(nodes)
  })

  it('cloneSelection：新 id + 位置偏移 + 仅克隆内部边 + 运行状态复位', () => {
    let seq = 0
    const makeId = () => `copy-${++seq}`
    const nodes = [bn('a', 0, 0, { args: ['-i', '{input}'] }), bn('b', 200, 0), bn('c', 400, 0)]
    const edges = [e('a', 'b'), e('b', 'c'), e('a', 'c')]
    const { nodes: cloned, edges: clonedEdges } = cloneSelection(
      nodes,
      edges,
      ['a', 'b'],
      makeId,
    )

    expect(cloned.map((n) => n.id)).toEqual(['copy-1', 'copy-2'])
    expect(cloned[0]?.position).toEqual({ x: CLONE_OFFSET.x, y: CLONE_OFFSET.y })
    expect(cloned.every((n) => !n.selected)).toBe(true)
    expect(cloned.every((n) => n.data.status === 'waiting')).toBe(true)
    // 原节点不受影响（params 深隔离）
    expect(nodes[0]?.data.params).toEqual({ args: ['-i', '{input}'] })
    // 仅 a->b 内部边被克隆（b->c、a->c 跨出选区）
    expect(clonedEdges).toEqual([
      {
        id: 'e-copy-1->copy-2',
        source: 'copy-1',
        target: 'copy-2',
        sourceHandle: 'out',
        targetHandle: 'in',
        selected: false,
      },
    ])
  })

  it('commonParamShape：同 builtin / 同模块同能力返回参数表，混合返回 null', () => {
    const twoFfmpeg = [bn('a', 0, 0), bn('b', 100, 0)]
    expect(commonParamShape(twoFfmpeg, ['a', 'b'])).not.toBeNull()

    const mixed = [bn('a', 0, 0), mn('m', 100, 0)]
    expect(commonParamShape(mixed, ['a', 'm'])).toBeNull()

    const twoModules = [mn('m1', 0, 0), mn('m2', 100, 0)]
    expect(commonParamShape(twoModules, ['m1', 'm2'])).not.toBeNull()

    // 不同能力（此处同模块同能力才成组；capabilityId 不同的 module 节点由
    // data 构造保证）——单节点不构成批量
    expect(commonParamShape(twoFfmpeg, ['a'])).toBeNull()
  })

  it('paramFieldStates：逐字段混合标记（值不一致 mixed=true）', () => {
    const nodes = [bn('a', 0, 0, { output_extension: 'mp4' }), bn('b', 100, 0, { output_extension: 'wav' })]
    const shape = commonParamShape(nodes, ['a', 'b'])!
    const states = paramFieldStates(nodes, ['a', 'b'], shape.specs)
    const ext = states.find((s) => s.spec.name === 'output_extension')
    expect(ext?.mixed).toBe(true)
    expect(ext?.value).toBe('mp4')
    // 未设置的参数在选区内一致（都 undefined → 不算混合）
    const args = states.find((s) => s.spec.name === 'args')
    expect(args?.mixed).toBe(false)
  })
})
