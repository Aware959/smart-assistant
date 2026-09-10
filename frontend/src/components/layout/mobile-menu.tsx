import { NavLink, useLocation } from 'react-router-dom'
import { Library, MessageSquareText } from 'lucide-react'
import { SessionsPanel } from '@/features/sessions/components/session-sidebar'
import { Sheet, SheetContent } from '@/components/ui/sheet'
import { useUiStore } from '@/stores/ui'
import { cn } from '@/lib/utils'

const NAV = [
  { to: '/', label: '对话', icon: MessageSquareText, end: true },
  { to: '/memories', label: '记忆', icon: Library, end: false },
]

export function MobileMenu() {
  const open = useUiStore((s) => s.mobileMenuOpen)
  const setOpen = useUiStore((s) => s.setMobileMenuOpen)
  const location = useLocation()

  return (
    <Sheet open={open} onOpenChange={setOpen}>
      <SheetContent className="pt-[calc(env(safe-area-inset-top)+1rem)]">
        <div className="px-6 pr-12">
          <span className="font-serif text-2xl leading-none font-bold tracking-wide">记忆库</span>
          <p className="mt-1.5 font-mono text-[10px] tracking-[0.32em] text-muted-foreground">
            SMART&nbsp;ASSISTANT
          </p>
        </div>

        <nav className="mt-6 flex flex-col gap-1 px-3">
          {NAV.map(({ to, label, icon: Icon, end }) => {
            const active = end ? location.pathname === to : location.pathname.startsWith(to)
            return (
              <NavLink
                key={to}
                to={to}
                end={end}
                onClick={() => setOpen(false)}
                className={cn(
                  'flex items-center gap-3 rounded-lg px-3 py-2.5 transition-colors',
                  active
                    ? 'bg-accent text-accent-foreground'
                    : 'text-foreground hover:bg-accent/60',
                )}
              >
                <Icon className="size-4.5 text-primary" />
                <span className="font-mono text-sm tracking-wider">{label}</span>
                <span className="sr-only">前往{label}页</span>
              </NavLink>
            )
          })}
        </nav>

        <div className="mt-4 h-px shrink-0 bg-border" />

        <div className="flex min-h-0 flex-1 flex-col">
          <SessionsPanel onNavigate={() => setOpen(false)} />
        </div>
      </SheetContent>
    </Sheet>
  )
}