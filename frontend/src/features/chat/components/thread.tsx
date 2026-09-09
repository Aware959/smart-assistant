'use client'

import { ThreadPrimitive } from '@assistant-ui/react'
import { AssistantMessage, UserMessage } from './messages'
import { ChatComposer } from './composer'

export function ChatThread() {
  return (
    <ThreadPrimitive.Root className="relative flex h-full flex-col overflow-hidden">
      <ThreadPrimitive.Viewport className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-5 px-4 pt-6 pb-32">
          <ThreadPrimitive.Empty>
            <div className="flex flex-col items-start gap-3 py-12">
              <span className="font-mono text-[11px] tracking-[0.4em] text-primary">
                LOCAL MEMORY ASSISTANT
              </span>
              <h1 className="font-serif text-4xl leading-tight font-semibold">
                把值得记住的，
                <br />
                留在本地。
              </h1>
              <p className="max-w-md text-sm leading-6 text-muted-foreground">
                回答会自动回顾本会话历史与已沉淀的记忆；值得记住的事实会实时落库，并在之后被语义召回。
              </p>
            </div>
          </ThreadPrimitive.Empty>

          <ThreadPrimitive.Messages components={{ UserMessage, AssistantMessage }} />
        </div>
      </ThreadPrimitive.Viewport>

      <div className="pointer-events-none absolute inset-x-0 bottom-0 h-28 bg-gradient-to-t from-background via-background/80 to-transparent" />

      <ChatComposer />
    </ThreadPrimitive.Root>
  )
}