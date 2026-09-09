'use client'

import { ComposerPrimitive, useAuiState } from '@assistant-ui/react'
import { SendHorizonal, Square } from 'lucide-react'

export function ChatComposer() {
  const isRunning = useAuiState((s) => s.thread.isRunning)

  return (
    <ComposerPrimitive.Root className="absolute inset-x-0 bottom-0 z-10">
      <div className="mx-auto w-full max-w-3xl px-4 pb-[calc(env(safe-area-inset-bottom)+1rem)]">
        <div className="flex items-end gap-2 rounded-2xl border border-input bg-card p-1.5 shadow-lg">
          <ComposerPrimitive.Input
            autoFocus={false}
            placeholder="说点什么…（Enter 发送，Shift+Enter 换行）"
            className="min-h-9 flex-1 resize-none bg-transparent px-2 py-2 text-[15px] leading-6 outline-none placeholder:text-muted-foreground"
          />
          <div className="flex items-center gap-1">
            {isRunning ? (
              <ComposerPrimitive.Cancel asChild>
                <button
                  type="button"
                  aria-label="停止生成"
                  className="flex size-9 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground transition-colors hover:bg-primary/90"
                >
                  <Square className="size-4 fill-current" />
                </button>
              </ComposerPrimitive.Cancel>
            ) : (
              <ComposerPrimitive.Send asChild>
                <button
                  type="button"
                  aria-label="发送"
                  className="flex size-9 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground transition-colors hover:bg-primary/90 disabled:opacity-40"
                >
                  <SendHorizonal className="size-4" />
                </button>
              </ComposerPrimitive.Send>
            )}
          </div>
        </div>
      </div>
    </ComposerPrimitive.Root>
  )
}