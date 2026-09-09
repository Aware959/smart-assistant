import { clsx, type ClassValue } from 'clsx'
import { twMerge } from 'tailwind-merge'

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

export function formatTime(iso: string | null | undefined): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

const MINUTE = 60_000
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

/** 人性化相对时间：刚刚 / n 分钟前 / 今天 HH:mm / 昨天 / n 天前 / M月D日 / YYYY/M/D */
export function formatRelativeTime(iso: string | null | undefined): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso

  const now = new Date()
  const diff = now.getTime() - d.getTime()

  if (diff < MINUTE) return '刚刚'
  if (diff < HOUR) return `${Math.floor(diff / MINUTE)} 分钟前`

  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate())
  const startOfDay = new Date(d.getFullYear(), d.getMonth(), d.getDate())
  const dayDiff = Math.round((startOfToday.getTime() - startOfDay.getTime()) / DAY)

  if (dayDiff === 0)
    return d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
  if (dayDiff === 1) return '昨天'
  if (dayDiff < 7) return `${dayDiff} 天前`
  if (d.getFullYear() === now.getFullYear())
    return d.toLocaleDateString('zh-CN', { month: 'numeric', day: 'numeric' })
  return d.toLocaleDateString('zh-CN', {
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
  })
}

/** 会话列表分组：返回按时间倒序的分组标签与条目。 */
export type SessionGroup<Item> = { label: string; items: Item[] }

export function groupByTime<Item extends { updated_at?: string | null }>(
  items: Item[],
): SessionGroup<Item>[] {
  const now = new Date()
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate())
  const groups: { label: string; exp: number; items: Item[] }[] = [
    { label: '今天', exp: 0, items: [] },
    { label: '昨天', exp: 1, items: [] },
    { label: '近 7 天', exp: 7, items: [] },
    { label: '近 30 天', exp: 30, items: [] },
    { label: '更早', exp: Infinity, items: [] },
  ]

  for (const it of items) {
    const d = new Date(it.updated_at ?? '')
    if (Number.isNaN(d.getTime())) {
      groups[groups.length - 1].items.push(it)
      continue
    }
    const dayDiff = Math.floor(
      (startOfToday.getTime() - new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()) /
        (24 * HOUR),
    )
    const g = groups.find((x) => dayDiff < x.exp) ?? groups[groups.length - 1]
    g.items.push(it)
  }

  return groups.filter((g) => g.items.length > 0)
}