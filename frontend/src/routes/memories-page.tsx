'use client'

import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { Library, Plus, Search, Trash2 } from 'lucide-react'
import {
  useCreateMemory,
  useDeleteMemory,
  useMemories,
  useSearchMemories,
} from '@/features/memories/hooks'
import { memoryFormSchema, type MemoryFormValues, type MemoryHit } from '@/schema'
import { useDebouncedValue } from '@/lib/use-debounced-value'
import { formatTime } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { ConfirmDialog } from '@/components/common/confirm-dialog'

export default function MemoriesPage() {
  const { data: memories, isLoading } = useMemories()
  const [query, setQuery] = useState('')
  const debounced = useDebouncedValue(query)
  const { data: hits } = useSearchMemories(debounced)
  const createMemory = useCreateMemory()
  const deleteMemory = useDeleteMemory()

  const form = useForm<MemoryFormValues>({
    resolver: zodResolver(memoryFormSchema),
    defaultValues: { content: '', memory_type: 'fact' },
  })

  const searching = debounced.trim().length > 0
  const list = searching ? hits ?? [] : memories ?? []

  return (
    <div className="mx-auto h-full w-full max-w-4xl overflow-y-auto px-4 py-6 md:px-6 md:py-8">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="flex items-center gap-2 font-serif text-2xl font-semibold md:text-3xl">
            <Library className="size-6 text-primary" />
            沉淀记忆
          </h1>
          <p className="mt-1 text-sm text-muted-foreground">
            对话中被判定值得记住的事实，会实时沉淀到这里。
          </p>
        </div>
        <Dialog>
          <DialogTrigger asChild>
            <Button>
              <Plus className="size-4" />
              添加记忆
            </Button>
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>手动沉淀一条记忆</DialogTitle>
              <DialogDescription>记忆会进入向量库，可被后续对话语义召回。</DialogDescription>
            </DialogHeader>
            <form
              className="grid gap-4"
              onSubmit={form.handleSubmit((values) =>
                createMemory.mutate(values, {
                  onSuccess: () => form.reset(),
                }),
              )}
            >
              <div className="grid gap-2">
                <Label htmlFor="memory-content">内容</Label>
                <Textarea
                  id="memory-content"
                  placeholder="例如：用户偏好把资料按主题归档到本地目录"
                  className="min-h-24"
                  {...form.register('content')}
                />
                {form.formState.errors.content && (
                  <p className="text-sm text-destructive">{form.formState.errors.content.message}</p>
                )}
              </div>
              <div className="grid gap-2">
                <Label htmlFor="memory-type">类型</Label>
                <Input id="memory-type" placeholder="fact / preference / item …" {...form.register('memory_type')} />
              </div>
              <DialogFooter>
                <Button type="submit" disabled={createMemory.isPending}>
                  {createMemory.isPending ? '保存中…' : '保存'}
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>
      </header>

      <div className="relative mt-6">
        <Search className="absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="语义检索记忆…"
          className="pl-9"
        />
      </div>

      <div className="mt-6 flex flex-col gap-3">
        {isLoading && Array.from({ length: 3 }).map((_, i) => <Skeleton key={i} className="h-20 w-full rounded-xl" />)}

        {!isLoading && !searching && list.length === 0 && (
          <p className="py-12 text-center font-mono text-sm text-muted-foreground">
            暂无记忆。去「对话」页聊两句，值得记住的会出现在这里。
          </p>
        )}

        {searching && list.length === 0 && (
          <p className="py-12 text-center font-mono text-sm text-muted-foreground">未检索到相关内容。</p>
        )}

        {list.map((m) => {
          const score = 'score' in m ? (m as MemoryHit).score : null
          return (
          <div key={m.id} className="group flex items-start justify-between gap-4 rounded-xl border bg-card px-4 py-3.5">
            <div className="min-w-0">
              <p className="text-[15px] leading-6">{m.content}</p>
              <div className="mt-2 flex items-center gap-2">
                <Badge variant="outline">{m.memory_type}</Badge>
                {score != null && (
                  <Badge variant="accent">{Math.round(score * 100)}%</Badge>
                )}
                <span className="font-mono text-[10px] text-muted-foreground">
                  {formatTime(m.created_at)}
                </span>
              </div>
            </div>
            <ConfirmDialog
              title="删除这条记忆？"
              description="删除后不再参与语义召回。"
              pending={deleteMemory.isPending}
              onConfirm={() => deleteMemory.mutate(m.id)}
              trigger={
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label="删除记忆"
                  className="text-muted-foreground opacity-0 hover:text-destructive group-hover:opacity-100"
                >
                  <Trash2 className="size-4" />
                </Button>
              }
            />
          </div>
          )
        })}
      </div>
    </div>
  )
}