import { Menu, Moon, Sun } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useTheme } from '@/lib/theme'
import { useUiStore } from '@/stores/ui'

export function MobileHeader() {
  const { resolved, setTheme } = useTheme()
  const setMobileMenuOpen = useUiStore((s) => s.setMobileMenuOpen)

  return (
    <header className="flex items-center justify-between border-b bg-card/40 px-4 pt-[calc(env(safe-area-inset-top)+0.625rem)] pb-3 md:hidden">
      <Button
        variant="ghost"
        size="icon"
        aria-label="打开菜单"
        onClick={() => setMobileMenuOpen(true)}
      >
        <Menu className="size-5" />
      </Button>

      <span className="font-serif text-lg leading-none font-bold tracking-wide">记忆库</span>

      <Button
        variant="ghost"
        size="icon"
        aria-label="切换主题"
        onClick={() => setTheme(resolved === 'dark' ? 'light' : 'dark')}
      >
        {resolved === 'dark' ? <Sun className="size-5" /> : <Moon className="size-5" />}
      </Button>
    </header>
  )
}