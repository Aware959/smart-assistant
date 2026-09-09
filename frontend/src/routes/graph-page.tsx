'use client'

import { Network, Trash2 } from 'lucide-react'
import { useDeleteEntity, useDeleteRelation, useEntities, useRelations } from '@/features/graph/hooks'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { ConfirmDialog } from '@/components/common/confirm-dialog'

export default function GraphPage() {
  const { data: entities, isLoading: loadingEntities } = useEntities()
  const { data: relations, isLoading: loadingRelations } = useRelations()
  const deleteEntity = useDeleteEntity()
  const deleteRelation = useDeleteRelation()

  return (
    <div className="mx-auto h-full w-full max-w-5xl overflow-y-auto px-4 py-6 md:px-6 md:py-8">
      <header>
        <h1 className="flex items-center gap-2 font-serif text-2xl font-semibold md:text-3xl">
          <Network className="size-6 text-primary" />
          知识图谱
        </h1>
        <p className="mt-1 text-sm text-muted-foreground mb-2">
          实体与关系来自对话中沉淀的记忆，构成可检索的知识网络。
        </p>
      </header>

      <div className="mt-4 grid grid-cols-1 gap-6 lg:grid-cols-2">
        <section>
          <h2 className="mb-3 font-mono text-xs tracking-[0.22em] text-primary uppercase">实体</h2>
          <div className="flex flex-col gap-2">
            {loadingEntities &&
              Array.from({ length: 3 }).map((_, i) => <Skeleton key={i} className="h-12 w-full rounded-lg" />)}
            {entities?.map((e) => (
              <div key={e.id} className="group flex items-center justify-between gap-3 rounded-lg border bg-card px-3 py-2.5">
                <div className="flex min-w-0 items-center gap-2">
                  <span className="truncate text-sm font-medium">{e.name}</span>
                  <Badge variant="outline" className="shrink-0">
                    {e.entity_type}
                  </Badge>
                </div>
                <ConfirmDialog
                  title="删除这个实体？"
                  description="关联该实体的关系也会一并移除。"
                  pending={deleteEntity.isPending}
                  onConfirm={() => deleteEntity.mutate(e.id)}
                  trigger={
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label="删除实体"
                      className="size-7 text-muted-foreground opacity-0 hover:text-destructive group-hover:opacity-100"
                    >
                      <Trash2 className="size-3.5" />
                    </Button>
                  }
                />
              </div>
            ))}
            {!loadingEntities && entities?.length === 0 && (
              <p className="py-8 text-center font-mono text-xs text-muted-foreground">暂无实体</p>
            )}
          </div>
        </section>

        <section>
          <h2 className="mb-3 font-mono text-xs tracking-[0.22em] text-primary uppercase">关系</h2>
          <div className="flex flex-col gap-2">
            {loadingRelations &&
              Array.from({ length: 3 }).map((_, i) => <Skeleton key={i} className="h-12 w-full rounded-lg" />)}
            {relations?.map((r) => (
              <div key={r.id} className="group flex items-center justify-between gap-3 rounded-lg border bg-card px-3 py-2.5">
                <div className="flex min-w-0 items-center gap-2">
                  <span className="truncate text-sm font-medium">{r.relation_type}</span>
                  <span className="shrink-0 font-mono text-[10px] text-muted-foreground">
                    {r.source_id.slice(0, 6)} ↦ {r.target_id.slice(0, 6)} · {r.weight}
                  </span>
                </div>
                <ConfirmDialog
                  title="删除这条关系？"
                  pending={deleteRelation.isPending}
                  onConfirm={() => deleteRelation.mutate(r.id)}
                  trigger={
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label="删除关系"
                      className="size-7 text-muted-foreground opacity-0 hover:text-destructive group-hover:opacity-100"
                    >
                      <Trash2 className="size-3.5" />
                    </Button>
                  }
                />
              </div>
            ))}
            {!loadingRelations && relations?.length === 0 && (
              <p className="py-8 text-center font-mono text-xs text-muted-foreground">暂无关系</p>
            )}
          </div>
        </section>
      </div>
    </div>
  )
}