'use client'

import { useCallback, useMemo } from 'react'
import { AssistantRuntimeProvider, useLocalRuntime, type ThreadMessageLike } from '@assistant-ui/react'
import { createChatAdapter } from './adapter'
import { useUiStore } from '@/stores/ui'
import { queryClient } from '@/lib/query-client'
import { sessionsKeys } from '@/features/sessions/hooks'
import type { Message } from '@/schema'

interface ChatRuntimeProps {
  sessionId: string | null
  messages: Message[]
  children: React.ReactNode
}

/**
 * 组装 assistant-ui 本地运行时：
 * - adapter 将 /chat/stream 的 SSE 接到 ChatModelAdapter（流式）
 * - 以当前会话的历史消息作为 initialMessages，会话切换时整体重建
 */
export function ChatRuntime({ sessionId, messages, children }: ChatRuntimeProps) {
  const setActiveSession = useUiStore((s) => s.setActiveSession)

  const onSessionResolved = useCallback(
    (newSessionId: string) => {
      setActiveSession(newSessionId)
      queryClient.invalidateQueries({ queryKey: sessionsKeys.all })
      void queryClient.invalidateQueries({ queryKey: sessionsKeys.messages(newSessionId) })
    },
    [setActiveSession],
  )

  const adapter = useMemo(
    () => createChatAdapter({ getSessionId: () => sessionId, onSessionResolved }),
    [sessionId, onSessionResolved],
  )

  const initialMessages: ThreadMessageLike[] = useMemo(
    () =>
      messages.map((m) => ({
        id: m.id,
        role: m.role,
        content: m.content,
        createdAt: new Date(m.created_at),
      })),
    [messages],
  )

  const runtime = useLocalRuntime(adapter, { initialMessages })

  return (
    <AssistantRuntimeProvider runtime={runtime}>
      <div className="h-full">{children}</div>
    </AssistantRuntimeProvider>
  )
}