import { NavLink, useNavigate } from 'react-router-dom'
import { Library, MessageSquareText, Moon, SquarePen, Sun } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useCreateSession } from '@/features/sessions/hooks'
import { useUiStore } from '@/stores/ui'
import { useTheme } from '@/lib/theme'
import { cn } from '@/lib/utils'

const NAV = [
  { to: '/', label: '对话', icon: MessageSquareText, end: true },
  { to: '/memories', label: '记忆', icon: Library, end: false },
]

export function NavRail() {
  const { resolved, setTheme } = useTheme()
  const navigate = useNavigate()
  const setActive = useUiStore((s) => s.setActiveSession)
  const createSession = useCreateSession()

  const handleNew = () => {
    createSession.mutate(undefined, {
      onSuccess: (s) => {
        setActive(s.id)
        navigate('/')
      },
    })
  }

  return (
    <nav className="flex h-full w-16 shrink-0 flex-col items-center border-r bg-card/40 py-3 lg:w-20">
      <span className="mb-3 font-serif text-lg font-bold text-primary" title="记忆库">
        <span className="lg:hidden">记</span>
        <span className="hidden lg:inline">记忆库</span>
      </span>

      <Button
        variant="ghost"
        size="icon"
        aria-label="新建会话"
        title="新建会话"
        disabled={createSession.isPending}
        onClick={handleNew}
        className="mb-3"
      >
        {createSession.isPending ? (
          <span className="size-4 animate-spin rounded-full border-2 border-primary/30 border-t-primary" />
        ) : (
          <SquarePen className="size-4.5" />
        )}
      </Button>

      <div className="flex flex-col gap-1.5">
        {NAV.map(({ to, label, icon: Icon, end }) => (
          <NavLink
            key={to}
            to={to}
            end={end}
            title={label}
            className={({ isActive }) =>
              cn(
                'flex flex-col items-center gap-1 rounded-lg px-1 py-2 transition-colors lg:px-3',
                isActive
                  ? 'bg-accent text-accent-foreground'
                  : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground',
              )
            }
          >
            <Icon className="size-4.5" />
            <span className="font-mono text-[10px] tracking-widest">{label}</span>
          </NavLink>
        ))}
      </div>

      <div className="mt-auto">
        <Button
          variant="ghost"
          size="icon"
          aria-label="切换主题"
          onClick={() => setTheme(resolved === 'dark' ? 'light' : 'dark')}
        >
          {resolved === 'dark' ? <Sun className="size-4" /> : <Moon className="size-4" />}
        </Button>
      </div>
    </nav>
  )
}