import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  createMemoryInputSchema,
  memorySchema,
  memoryHitSchema,
  searchQuerySchema,
  parseOrThrow,
  z,
  type CreateMemoryInput,
} from '@/schema'
import { http } from '@/lib/http'

export const memoriesKeys = {
  all: ['memories'] as const,
  search: (query: string) => ['memories', 'search', query] as const,
}

export function useMemories() {
  return useQuery({
    queryKey: memoriesKeys.all,
    queryFn: async () => {
      const { data } = await http.get('/memories')
      return parseOrThrow(z.array(memorySchema), data)
    },
  })
}

export function useCreateMemory() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: async (input: CreateMemoryInput) => {
      const parsed = createMemoryInputSchema.parse(input)
      const { data } = await http.post('/memory', {
        content: parsed.content,
        memory_type: parsed.memory_type ?? 'fact',
        message_id: parsed.message_id ?? null,
      })
      return parseOrThrow(memorySchema, data)
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: memoriesKeys.all })
      toast.success('记忆已沉淀')
    },
    onError: (err) => toast.error((err as Error).message),
  })
}

export function useDeleteMemory() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: async (id: string) => {
      await http.delete(`/memory/${id}`)
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: memoriesKeys.all })
      toast.success('记忆已删除')
    },
    onError: (err) => toast.error((err as Error).message),
  })
}

export function useSearchMemories(query: string) {
  const trimmed = query.trim()
  return useQuery({
    queryKey: memoriesKeys.search(trimmed),
    queryFn: async () => {
      const input = searchQuerySchema.parse({ query: trimmed })
      const { data } = await http.post('/search', input)
      return parseOrThrow(z.array(memoryHitSchema), data)
    },
    enabled: trimmed.length > 0,
    placeholderData: (prev) => prev,
  })
}