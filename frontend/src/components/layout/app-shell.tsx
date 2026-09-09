import { Outlet } from 'react-router-dom'
import { MobileHeader } from './mobile-header'
import { MobileMenu } from './mobile-menu'
import { NavRail } from './nav-rail'

export function AppShell() {
  return (
    <div className="flex h-full flex-col">
      <MobileHeader />
      <MobileMenu />
      <div className="flex min-h-0 flex-1">
        <div className="hidden h-full md:flex">
          <NavRail />
        </div>
        <div className="min-w-0 flex-1">
          <Outlet />
        </div>
      </div>
    </div>
  )
}