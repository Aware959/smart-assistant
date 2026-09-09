'use client'

import { useEffect } from 'react'
import { LoaderCircle } from 'lucide-react'
import { useSessions, useSessionMessages } from '@/features/sessions/hooks'
import { useUiStore } from '@/stores/ui'
import { ChatRuntime } from '@/features/chat/runtime'
import { ChatHeader } from '@/features/chat/components/chat-header'
import { ChatThread } from '@/features/chat/components/thread'
import { SessionSidebar } from '@/features/sessions/components/session-sidebar'

export default function ChatPage() {
  const activeId = useUiStore((s) => s.activeSessionId)
  const setActive = useUiStore((s) => s.setActiveSession)
  const { data: sessions, isSuccess } = useSessions()
  const { data: messages = [], isLoading: messagesLoading } = useSessionMessages(activeId)

  useEffect(() => {
    if (isSuccess && sessions && sessions.length > 0 && !activeId) {
      setActive(sessions[0].id)
    }
  }, [isSuccess, sessions, activeId, setActive])

  return (
    <div className="flex h-full">
      <SessionSidebar />
      <main className="flex min-w-0 flex-1 flex-col">
        <ChatHeader />
        <div className="min-h-0 flex-1">
          {activeId != null && messagesLoading ? (
            <div className="grid h-full place-items-center">
              <LoaderCircle className="size-6 animate-spin text-muted-foreground" />
            </div>
          ) : (
            <ChatRuntime
              key={activeId ?? 'empty'}
              sessionId={activeId}
              messages={isSuccess ? messages : []}
            >
              <ChatThread />
            </ChatRuntime>
          )}
        </div>
      </main>
    </div>
  )
}