'use client'

import { useMemo, useState } from 'react'
import { EllipsisVertical, Plus, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { useCreateSession, useDeleteSession, useSessions } from '@/features/sessions/hooks'
import { useUiStore } from '@/stores/ui'
import { cn, formatRelativeTime, groupByTime } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
import type { Session } from '@/schema'

interface SessionsPanelProps {
  onNavigate?: () => void
}

export function SessionsPanel({ onNavigate }: SessionsPanelProps) {
  const { data: sessions, isLoading } = useSessions()
  const activeId = useUiStore((s) => s.activeSessionId)
  const setActive = useUiStore((s) => s.setActiveSession)
  const clearActive = useUiStore((s) => s.clearActiveSession)
  const createSession = useCreateSession()
  const deleteSession = useDeleteSession()
  const [menuFor, setMenuFor] = useState<string | null>(null)
  const [confirmId, setConfirmId] = useState<string | null>(null)

  const groups = useMemo(() => groupByTime<Session>(sessions ?? []), [sessions])

  const select = (id: string) => {
    setMenuFor(null)
    setActive(id)
    onNavigate?.()
  }

  const handleDelete = (id: string) => {
    setConfirmId(null)
    deleteSession.mutate(id, {
      onSuccess: () => {
        toast.success('会话已删除')
        if (id === activeId) {
          const next = sessions?.find((x) => x.id !== id)
          if (next) setActive(next.id)
          else clearActive()
        }
      },
      onError: (e) => toast.error(e.message),
    })
  }

  const handleNew = () => {
    createSession.mutate(undefined, {
      onSuccess: (s) => {
        select(s.id)
        toast.success('会话已创建')
      },
      onError: (e) => toast.error(e.message),
    })
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex flex-col gap-2 px-3 pt-3 pb-2">
        <Button
          size="sm"
          className="h-8 justify-start gap-1.5 rounded-lg px-2.5 text-[13px] font-medium"
          disabled={createSession.isPending}
          onClick={handleNew}
        >
          <Plus className="size-4" />
          {createSession.isPending ? '创建中…' : '新建会话'}
        </Button>
      </div>

      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col px-1.5 pb-3">
          {isLoading &&
            Array.from({ length: 5 }).map((_, i) => (
              <Skeleton key={i} className="my-0.5 h-9 w-full rounded-md" />
            ))}

          {!isLoading &&
            groups.map((g) => (
              <div key={g.label} className="mt-2 first:mt-0">
                <p className="px-2.5 py-1 font-mono text-[10px] tracking-[0.18em] text-muted-foreground uppercase">
                  {g.label}
                  <span className="ml-1.5 text-muted-foreground/50">{g.items.length}</span>
                </p>
                <div className="mt-0.5 flex flex-col gap-px">
                  {g.items.map((s) => {
                    const active = s.id === activeId

                    if (confirmId === s.id) {
                      return (
                        <div
                          key={s.id}
                          className="mx-0.5 rounded-md border border-destructive/50 bg-destructive/5 px-2 py-1.5"
                        >
                          <p className="font-mono text-[11px] text-muted-foreground">
                            确定删除这个会话？
                          </p>
                          <div className="mt-1.5 flex gap-1.5">
                            <Button
                              variant="destructive"
                              size="sm"
                              className="h-6 px-2.5 text-xs"
                              disabled={deleteSession.isPending && deleteSession.variables === s.id}
                              onClick={() => handleDelete(s.id)}
                            >
                              {deleteSession.isPending && deleteSession.variables === s.id
                                ? '删除中…'
                                : '删除'}
                            </Button>
                            <Button
                              variant="outline"
                              size="sm"
                              className="h-6 px-2.5 text-xs"
                              disabled={deleteSession.isPending}
                              onClick={(e) => {
                                e.stopPropagation()
                                setConfirmId(null)
                              }}
                            >
                              取消
                            </Button>
                          </div>
                        </div>
                      )
                    }

                    return (
                      <div
                        key={s.id}
                        onClick={() => select(s.id)}
                        className={cn(
                          'group relative flex cursor-pointer items-center gap-1 rounded-md py-1.5 pr-1 pl-2.5 transition-colors',
                          active
                            ? 'bg-accent text-accent-foreground'
                            : 'hover:bg-accent/50 text-foreground',
                        )}
                      >
                        <div className="min-w-0 flex-1">
                          <div className="truncate text-[13px] leading-tight font-medium">
                            {s.title || '新会话'}
                          </div>
                          <div className="truncate text-[11px] leading-tight text-muted-foreground">
                            {formatRelativeTime(s.updated_at) || '—'}
                          </div>
                        </div>

                        <div className="relative shrink-0">
                          <button
                            aria-label="更多操作"
                            onClick={(e) => {
                              e.stopPropagation()
                              setMenuFor((cur) => (cur === s.id ? null : s.id))
                            }}
                            className={cn(
                              'flex size-6 items-center justify-center rounded text-muted-foreground transition-opacity hover:bg-foreground/10 hover:text-foreground',
                              menuFor === s.id
                                ? 'bg-foreground/10 text-foreground opacity-100'
                                : 'opacity-0 group-hover:opacity-100',
                            )}
                          >
                            <EllipsisVertical className="size-3.5" />
                          </button>

                          {menuFor === s.id && (
                            <div
                              onClick={(e) => e.stopPropagation()}
                              className="absolute top-full right-0 z-20 mt-1 min-w-28 overflow-hidden rounded-md border bg-popover p-1 text-popover-foreground shadow-md"
                            >
                              <button
                                onClick={() => {
                                  setMenuFor(null)
                                  setConfirmId(s.id)
                                }}
                                className="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-[13px] text-destructive transition-colors hover:bg-destructive hover:text-destructive-foreground"
                              >
                                <Trash2 className="size-3.5" />
                                删除会话
                              </button>
                            </div>
                          )}
                        </div>
                      </div>
                    )
                  })}
                </div>
              </div>
            ))}

          {!isLoading && groups.length === 0 && (
            <p className="px-3 py-8 text-center font-mono text-xs text-muted-foreground">
              尚无会话，点击「新建会话」开始
            </p>
          )}
        </div>
      </ScrollArea>
    </div>
  )
}

export function SessionSidebar() {
  return (
    <aside className="hidden h-full w-80 shrink-0 flex-col border-r bg-card/40 md:flex">
      <SessionsPanel />
    </aside>
  )
}