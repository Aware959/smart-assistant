'use client'

import { useSessions } from '@/features/sessions/hooks'
import { useUiStore } from '@/stores/ui'
import { Badge } from '@/components/ui/badge'

export function ChatHeader() {
  const { data: sessions } = useSessions()
  const activeId = useUiStore((s) => s.activeSessionId)
  const session = sessions?.find((s) => s.id === activeId)

  return (
    <header className="hidden items-center justify-between gap-3 border-b bg-card/40 px-4 py-2.5 md:flex md:px-5 md:py-3">
      <div className="min-w-0">
        <h2 className="truncate text-base leading-tight font-semibold md:font-serif md:text-lg">
          {session?.title || '新会话'}
        </h2>
        <p className="mt-0.5 font-mono text-[10px] tracking-wide text-muted-foreground">
          session {activeId ? activeId.slice(0, 12) : '—'}
        </p>
      </div>
      <div className="hidden shrink-0 gap-2 sm:flex">
        <Badge variant="secondary">历史上下文 6</Badge>
        <Badge variant="accent">流式输出</Badge>
      </div>
    </header>
  )
}