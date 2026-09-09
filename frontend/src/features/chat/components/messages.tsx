'use client'

import { MessagePrimitive, useAuiState } from '@assistant-ui/react'
import { MarkdownTextPrimitive } from '@assistant-ui/react-markdown'

function ThinkingDots() {
  return (
    <svg
      width="24"
      height="10"
      viewBox="0 0 24 10"
      fill="currentColor"
      aria-label="正在思考"
      className="text-muted-foreground"
    >
      <g>
        <circle cx="4" cy="5" r="2.4" className="animate-bounce" style={{ animationDelay: '0ms' }} />
        <circle cx="12" cy="5" r="2.4" className="animate-bounce" style={{ animationDelay: '130ms' }} />
        <circle cx="20" cy="5" r="2.4" className="animate-bounce" style={{ animationDelay: '260ms' }} />
      </g>
    </svg>
  )
}

export function AssistantMessage() {
  const status = useAuiState((s) => s.message.status)
  const hasPart = useAuiState((s) => s.message.parts.length > 0)
  const thinking = status?.type === 'running' && !hasPart

  return (
    <MessagePrimitive.Root className="flex w-full justify-start">
      <div className="min-w-0 max-w-[85%] rounded-2xl rounded-tl-sm border bg-card px-4 py-3 text-[15px] leading-7 md:max-w-[70%]">
        {thinking ? (
          <ThinkingDots />
        ) : (
          <MessagePrimitive.Parts
            components={{
              Text: () => (
                <MarkdownTextPrimitive className="prose prose-sm max-w-none dark:prose-invert prose-p:leading-7" />
              ),
            }}
          />
        )}
      </div>
    </MessagePrimitive.Root>
  )
}

export function UserMessage() {
  return (
    <MessagePrimitive.Root className="flex w-full justify-end">
      <div className="min-w-0 max-w-[85%] rounded-2xl rounded-tr-sm bg-primary px-4 py-3 text-[15px] leading-7 text-primary-foreground md:max-w-[70%]">
        <MessagePrimitive.Parts />
      </div>
    </MessagePrimitive.Root>
  )
}