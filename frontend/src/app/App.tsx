import { lazy, Suspense } from 'react'
import { Route, Routes } from 'react-router-dom'
import { AppShell } from '@/components/layout/app-shell'
import { PageLoader } from '@/components/layout/page-loader'

const ChatPage = lazy(() => import('@/routes/chat-page'))
const MemoriesPage = lazy(() => import('@/routes/memories-page'))

export default function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route
          index
          element={
            <Suspense fallback={<PageLoader />}>
              <ChatPage />
            </Suspense>
          }
        />
        <Route
          path="memories"
          element={
            <Suspense fallback={<PageLoader />}>
              <MemoriesPage />
            </Suspense>
          }
        />
      </Route>
    </Routes>
  )
}