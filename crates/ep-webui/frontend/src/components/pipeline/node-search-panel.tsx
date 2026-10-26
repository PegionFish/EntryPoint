import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Search, Waypoints, X } from 'lucide-react'

import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { BUILTIN_DEFS, NODE_STATUS_META } from '@/components/shared/pipeline-node'
import type { PipelineFlowNode } from '@/components/shared/pipeline-node'
import { cn } from '@/lib/utils'

/** 节点类型展示名（builtin 用定义名；module 用「模块 · 能力」；纯函数） */
function nodeTypeLabel(n: PipelineFlowNode): string {
  if (n.data.kind === 'builtin') return BUILTIN_DEFS[n.data.builtin].label
  if (n.data.kind === 'module') return `${n.data.moduleId} · ${n.data.capabilityLabel}`
  return 'API'
}

/**
 * T-28b 侧栏节点搜索：按名称/类型过滤画布节点，点击结果 → 画布聚焦该节点
 * （页面层负责选中 + fitView）。收起为一个小搜索按钮，展开为搜索框 + 结果列表。
 */
export function NodeSearchPanel({
  nodes,
  onSelect,
  onClose,
}: {
  nodes: PipelineFlowNode[]
  onSelect: (id: string) => void
  onClose: () => void
}) {
  const { t } = useTranslation('pipeline')
  const [query, setQuery] = useState('')

  const results = useMemo(() => {
    const q = query.trim().toLowerCase()
    const list = nodes.map((n) => ({
      node: n,
      type: nodeTypeLabel(n),
    }))
    if (!q) return list.slice(0, 12)
    return list
      .filter(
        ({ node, type }) =>
          node.data.label.toLowerCase().includes(q) ||
          node.id.toLowerCase().includes(q) ||
          type.toLowerCase().includes(q),
      )
      .slice(0, 12)
  }, [nodes, query])

  return (
    <div
      className="glass w-60 rounded-lg border border-border-glow shadow-md"
      data-testid="node-search-panel"
    >
      <div className="flex items-center gap-1.5 px-2 py-1.5">
        <Search className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t('nodeSearch.placeholder', {
            defaultValue: '搜索节点（名称 / 类型 / id）',
          })}
          className="h-7 border-none bg-transparent px-1 text-xs shadow-none focus-visible:ring-0"
          aria-label={t('nodeSearch.placeholder', { defaultValue: '搜索节点' })}
        />
        <Button
          variant="ghost"
          size="icon-xs"
          onClick={onClose}
          aria-label={t('nodeSearch.closeAria', { defaultValue: '关闭节点搜索' })}
          title={t('nodeSearch.closeAria', { defaultValue: '关闭节点搜索' })}
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>

      {query.trim() !== '' && results.length === 0 && (
        <p className="px-3 pb-2 text-[11px] text-muted-foreground">
          {t('nodeSearch.empty', { defaultValue: '无匹配节点' })}
        </p>
      )}

      <div className="max-h-64 overflow-y-auto px-1.5 pb-1.5">
        {results.map(({ node, type }) => {
          const meta = NODE_STATUS_META[node.data.status]
          return (
            <button
              key={node.id}
              type="button"
              onClick={() => onSelect(node.id)}
              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-accent"
              title={node.id}
            >
              <span className={cn('h-1.5 w-1.5 shrink-0 rounded-full', meta.dot)} />
              <span className="min-w-0 flex-1 truncate text-xs">{node.data.label}</span>
              <span className="max-w-24 shrink-0 truncate rounded bg-muted px-1 py-px text-[9px] text-muted-foreground">
                {type}
              </span>
            </button>
          )
        })}
      </div>

      {nodes.length === 0 && (
        <p className="flex items-center gap-1.5 px-3 pb-2 text-[11px] text-muted-foreground">
          <Waypoints className="h-3 w-3" />
          {t('nodeSearch.emptyCanvas', { defaultValue: '画布暂无节点' })}
        </p>
      )}
    </div>
  )
}
