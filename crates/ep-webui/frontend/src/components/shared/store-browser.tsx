import { useCallback, useEffect, useMemo, useState } from 'react'
import { ChevronRight, FolderOpen, Loader2 } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { api } from '@/api/client'
import type { StoreBrowseEntry, StoreInfo } from '@/api/types'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { cn } from '@/lib/utils'

/** 目录条目行（dir 排前由服务端保证；此处保持顺序渲染） */
function EntryRow({
  entry,
  onOpen,
}: {
  entry: StoreBrowseEntry
  onOpen: (name: string) => void
}) {
  return (
    <button
      type="button"
      disabled={entry.kind !== 'dir'}
      onClick={() => onOpen(entry.name)}
      className={cn(
        'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm',
        entry.kind === 'dir'
          ? 'hover:bg-muted/60'
          : 'cursor-default text-muted-foreground',
      )}
    >
      {entry.kind === 'dir' ? (
        <FolderOpen className="h-4 w-4 shrink-0 text-muted-foreground" />
      ) : (
        <span className="h-4 w-4 shrink-0" />
      )}
      <span className="min-w-0 flex-1 truncate">{entry.name}</span>
      {entry.kind === 'file' && entry.size > 0 ? (
        <span className="shrink-0 text-[11px] text-muted-foreground">
          {formatBytes(entry.size)}
        </span>
      ) : null}
    </button>
  )
}

/** 字节数 → 人类可读（KB/MB/GB；粗粒度展示足够） */
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`
}

/**
 * 存储池目录浏览对话框（T-26）。
 *
 * 服务端 GET /api/stores/{name}/browse 单层浏览（`..` 穿越与绝对注入全拒绝），
 * 本组件只做导航壳：选池 → 逐层进目录 → 「选择当前目录」返回
 * `store:<name>/<相对路径>`（池根为 `store:<name>`）。
 * 物理路径向后兼容：用户仍可手填物理路径，本对话框只是便捷入口。
 */
export function StoreBrowserDialog({
  open,
  onOpenChange,
  onPicked,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** 选中目录的 `store:` 逻辑名引用（池根无相对部分时为 `store:<name>`） */
  onPicked: (storeRef: string) => void
}) {
  const { t, i18n } = useTranslation('pipeline')
  const [stores, setStores] = useState<StoreInfo[] | null>(null)
  const [storeName, setStoreName] = useState('')
  const [relPath, setRelPath] = useState('')
  const [entries, setEntries] = useState<StoreBrowseEntry[] | null>(null)
  const [loading, setLoading] = useState(false)

  // 打开时加载池列表（仅启用的池可选）
  useEffect(() => {
    if (!open) return
    setStores(null)
    setEntries(null)
    setRelPath('')
    api
      .listStores()
      .then((resp) => {
        const enabled = resp.stores.filter((s) => s.enabled)
        setStores(enabled)
        setStoreName((prev) => prev || enabled[0]?.name || '')
      })
      .catch(() => {
        setStores([])
        toast.error(t('storeBrowser.loadFailed'))
      })
  }, [open, t])

  const load = useCallback(
    (name: string, path: string) => {
      if (!name) return
      setLoading(true)
      api
        .browseStore(name, path)
        .then((resp) => {
          setEntries(resp.entries)
          setRelPath(resp.path ?? '')
        })
        .catch((err) => {
          setEntries([])
          const msg = err instanceof Error ? err.message : String(err)
          const bodyStart = msg.indexOf(': ')
          toast.error(t('storeBrowser.loadFailed'), {
            description: bodyStart === -1 ? msg : msg.slice(bodyStart + 2),
          })
        })
        .finally(() => setLoading(false))
    },
    [t],
  )

  useEffect(() => {
    if (open && storeName) load(storeName, relPath)
    // 打开或切换池时重载；relPath 由 load 回写，避免循环
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, storeName])

  /** 相对路径分段（服务端归一为 / 分隔） */
  const crumbs = useMemo(() => relPath.split('/').filter(Boolean), [relPath])

  const openDir = (segment: string) => {
    const next = relPath ? `${relPath}/${segment}` : segment
    if (storeName) load(storeName, next)
  }

  const upTo = (index: number) => {
    // -1 = 池根
    const next = crumbs.slice(0, index + 1).join('/')
    if (storeName) load(storeName, next)
  }

  const pick = () => {
    onPicked(relPath ? `store:${storeName}/${relPath}` : `store:${storeName}`)
    onOpenChange(false)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('storeBrowser.title')}</DialogTitle>
          <DialogDescription className="sr-only">
            {t('storeBrowser.title')}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <label className="text-sm font-medium">{t('storeBrowser.store')}</label>
            <Select
              value={storeName || undefined}
              onValueChange={(v) => {
                setStoreName(v)
                setRelPath('')
                setEntries(null)
              }}
            >
              <SelectTrigger className="w-full">
                <SelectValue placeholder={t('storeBrowser.storePlaceholder')} />
              </SelectTrigger>
              <SelectContent>
                {(stores ?? []).map((s) => (
                  <SelectItem key={s.name} value={s.name}>
                    {s.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {stores !== null && stores.length === 0 ? (
              <p className="text-[11px] text-muted-foreground">
                {t('storeBrowser.noStores')}
              </p>
            ) : null}
          </div>

          {/* 面包屑：池根 / a / b */}
          <div className="flex flex-wrap items-center gap-1 rounded-md border border-border bg-muted/30 px-2 py-1.5 text-xs">
            <button
              type="button"
              className="rounded px-1 hover:bg-muted"
              onClick={() => upTo(-1)}
            >
              {storeName || '…'}
            </button>
            {crumbs.map((seg, idx) => (
              <span key={idx} className="flex items-center gap-1">
                <ChevronRight className="h-3 w-3 text-muted-foreground" />
                <button
                  type="button"
                  className="rounded px-1 hover:bg-muted"
                  onClick={() => upTo(idx)}
                >
                  {seg}
                </button>
              </span>
            ))}
          </div>

          <div className="h-64 overflow-y-auto rounded-md border border-border">
            {loading && entries === null ? (
              <div className="flex items-center justify-center py-10 text-muted-foreground">
                <Loader2 className="h-5 w-5 animate-spin" />
              </div>
            ) : entries !== null && entries.length === 0 ? (
              <p className="py-10 text-center text-sm text-muted-foreground">
                {t('storeBrowser.empty')}
              </p>
            ) : (
              <div className="p-1">
                {(entries ?? []).map((e) => (
                  <EntryRow key={`${e.kind}:${e.name}`} entry={e} onOpen={openDir} />
                ))}
              </div>
            )}
          </div>

          <p className="text-[11px] text-muted-foreground">
            {t('storeBrowser.hint', {
              value: relPath ? `store:${storeName}/${relPath}` : `store:${storeName}`,
            })}
          </p>
          <p className="text-[11px] text-muted-foreground" dir={i18n.language === 'zh-CN' ? undefined : 'ltr'}>
            {t('storeBrowser.serverHint')}
          </p>
        </div>

        <DialogFooter className="gap-2">
          <Button variant="ghost" size="sm" onClick={() => onOpenChange(false)}>
            {t('common:action.cancel')}
          </Button>
          <Button size="sm" onClick={pick} disabled={!storeName}>
            {t('storeBrowser.pick')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
