import { z } from 'zod'

export { z }

export const sessionSchema = z.object({
  id: z.string(),
  title: z.string(),
  created_at: z.string(),
  updated_at: z.string(),
})
export type Session = z.infer<typeof sessionSchema>

export const messageSchema = z.object({
  id: z.string(),
  session_id: z.string(),
  role: z.enum(['user', 'assistant']),
  content: z.string(),
  created_at: z.string(),
})
export type Message = z.infer<typeof messageSchema>

export const memorySchema = z.object({
  id: z.string(),
  content: z.string(),
  memory_type: z.string(),
  message_id: z.string().nullable(),
  created_at: z.string(),
  updated_at: z.string(),
})
export type Memory = z.infer<typeof memorySchema>

export const memoryHitSchema = memorySchema.extend({
  score: z.number(),
})
export type MemoryHit = z.infer<typeof memoryHitSchema>

export const chatOutputSchema = z.object({
  session_id: z.string(),
  reply: z.string(),
  memory: memorySchema.nullable(),
})
export type ChatOutput = z.infer<typeof chatOutputSchema>

export const memoryFormSchema = z.object({
  content: z.string().trim().min(1, '内容不能为空'),
  memory_type: z.string().trim().min(1).max(32),
})
export type MemoryFormValues = z.infer<typeof memoryFormSchema>

export const chatRequestSchema = z.object({
  message: z.string().trim().min(1, '消息不能为空'),
  session_id: z.string().nullable().optional(),
  history_count: z.number().int().min(0).max(64).nullable().optional(),
})
export type ChatRequest = z.infer<typeof chatRequestSchema>

export const createMemoryInputSchema = z.object({
  content: z.string().trim().min(1, '内容不能为空'),
  memory_type: z.string().trim().min(1).max(32).optional(),
  message_id: z.string().nullable().optional(),
})
export type CreateMemoryInput = z.infer<typeof createMemoryInputSchema>

export const searchQuerySchema = z.object({
  query: z.string().trim().min(1, '搜索词不能为空'),
  limit: z.number().int().min(1).max(20).optional().default(6),
})
export type SearchQuery = z.infer<typeof searchQuerySchema>

export function parseOrThrow<T>(schema: z.ZodType<T>, data: unknown): T {
  const result = schema.safeParse(data)
  if (!result.success) {
    throw new Error(result.error.issues.map((i) => i.message).join('；'))
  }
  return result.data
}