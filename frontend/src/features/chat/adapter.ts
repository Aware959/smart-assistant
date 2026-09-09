import type { ChatModelAdapter, ThreadMessage } from '@assistant-ui/react'
import { chatRequestSchema, chatOutputSchema, type ChatOutput } from '@/schema'
import { readSse } from './sse'

export interface ChatAdapterContext {
  /** 当前会话 ID（首轮为 null，由服务端新建会话后回传） */
  getSessionId: () => string | null
  /** 首轮 done 事件携带服务端生成的新会话 ID */
  onSessionResolved: (sessionId: string, output: ChatOutput) => void
}

const ABORTED = new DOMException('用户中断', 'AbortError')

function lastUserText(messages: readonly ThreadMessage[]): string | null {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i]
    if (m.role !== 'user') continue
    const text = m.content
      .map((p) => (p.type === 'text' ? p.text : ''))
      .join('')
    if (text.trim()) return text
  }
  return null
}

/**
 * 将 /chat/stream 的 OpenAI 兼容 SSE 接入 assistant-ui 的 ChatModelAdapter：
 * 每个 agent 流式运行期间持续 yield 累积文本，实现逐字呈现。
 */
export function createChatAdapter(ctx: ChatAdapterContext): ChatModelAdapter {
  return {
    async *run({ messages, abortSignal }) {
      const text = lastUserText(messages)
      if (!text) {
        yield { content: [] }
        return
      }

      const request = chatRequestSchema.parse({
        message: text,
        session_id: ctx.getSessionId(),
        history_count: null,
      })

      const res = await fetch('/chat/stream', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(request),
        signal: abortSignal,
      })
      if (!res.ok || !res.body) {
        throw new Error(`请求失败 HTTP ${res.status}`)
      }

      let buffer = ''
      let error: string | null = null
      let output: ChatOutput | null = null

      for await (const frame of readSse(res.body, abortSignal)) {
        try {
          if (frame.data === '[DONE]') continue
          const parsed = JSON.parse(frame.data) as Record<string, unknown>

          if (frame.event === 'done') {
            if (parsed.type === 'error') {
              error = String(parsed.message ?? '未知错误')
            } else {
              output = chatOutputSchema.parse(parsed)
              ctx.onSessionResolved(output.session_id, output)
            }
            continue
          }

          const delta = (parsed.choices as Array<{ delta?: { content?: string } }> | undefined)?.[0]?.delta?.content
          if (!delta) continue
          buffer += delta
          yield { content: [{ type: 'text', text: buffer }] }
        } catch {
          // 忽略无法解析的帧（keep-alive 等）
        }
      }

      if (error) throw new Error(error)
      if (abortSignal.aborted) throw ABORTED

      yield {
        content: [{ type: 'text', text: buffer }],
        metadata: {
          custom: output
            ? {
                sessionId: output.session_id,
                memorySaved: output.memory !== null,
                entities: output.entities.length,
                relations: output.relations.length,
              }
            : undefined,
        },
      }
      return
    },
  }
}