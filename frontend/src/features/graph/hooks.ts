import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { entitySchema, relationSchema, parseOrThrow, z } from '@/schema'
import { http } from '@/lib/http'

export const graphKeys = {
  all: ['graph'] as const,
  entities: ['graph', 'entities'] as const,
  relations: ['graph', 'relations'] as const,
}

export function useEntities() {
  return useQuery({
    queryKey: graphKeys.entities,
    queryFn: async () => {
      const { data } = await http.get('/entities')
      return parseOrThrow(z.array(entitySchema), data)
    },
  })
}

export function useRelations() {
  return useQuery({
    queryKey: graphKeys.relations,
    queryFn: async () => {
      const { data } = await http.get('/relations')
      return parseOrThrow(z.array(relationSchema), data)
    },
  })
}

export function useDeleteEntity() {
  const qc = useQueryClient()
  const invalidate = () => {
    qc.invalidateQueries({ queryKey: graphKeys.entities })
    qc.invalidateQueries({ queryKey: graphKeys.relations })
  }
  return useMutation({
    mutationFn: async (id: string) => {
      await http.delete(`/entities/${id}`)
    },
    onSuccess: () => {
      invalidate()
      toast.success('实体已删除')
    },
    onError: (err) => toast.error((err as Error).message),
  })
}

export function useDeleteRelation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: async (id: string) => {
      await http.delete(`/relations/${id}`)
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: graphKeys.relations })
      toast.success('关系已删除')
    },
    onError: (err) => toast.error((err as Error).message),
  })
}