import { Skeleton } from '@/components/ui/skeleton'

export function PageLoader() {
  return (
    <div className="flex h-full flex-col gap-3 p-6">
      <Skeleton className="h-8 w-48 rounded-lg" />
      <Skeleton className="h-4 w-72 rounded-lg" />
      <Skeleton className="mt-4 h-full w-full rounded-xl" />
    </div>
  )
}