import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { sessionSchema, messageSchema, parseOrThrow, z } from '@/schema'
import { http } from '@/lib/http'

export const sessionsKeys = {
  all: ['sessions'] as const,
  messages: (sessionId: string | null) => ['sessions', sessionId, 'messages'] as const,
}

export function useSessions() {
  return useQuery({
    queryKey: sessionsKeys.all,
    queryFn: async () => {
      const { data } = await http.get('/sessions')
      return parseOrThrow(z.array(sessionSchema), data)
    },
  })
}

export function useCreateSession() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: async () => {
      const { data } = await http.post('/sessions', {})
      return parseOrThrow(sessionSchema, data)
    },
    onSuccess: (session) => {
      qc.setQueryData(sessionsKeys.all, (old: Session[] | undefined) =>
        old ? [session, ...old] : [session],
      )
    },
  })
}

export function useDeleteSession() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: async (id: string) => {
      await http.delete(`/sessions/${id}`)
    },
    onSuccess: (_data, id) => {
      qc.setQueryData(sessionsKeys.all, (old: Session[] | undefined) =>
        old?.filter((s) => s.id !== id) ?? [],
      )
      qc.removeQueries({ queryKey: sessionsKeys.messages(id) })
    },
  })
}

export function useSessionMessages(sessionId: string | null) {
  return useQuery({
    queryKey: sessionsKeys.messages(sessionId),
    queryFn: async () => {
      const { data } = await http.get(`/sessions/${sessionId}/messages`)
      return parseOrThrow(z.array(messageSchema), data)
    },
    enabled: sessionId !== null,
  })
}

import type { Session } from '@/schema'