import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import { QueryClientProvider } from '@tanstack/react-query'
import { Toaster } from 'sonner'
import App from './App'
import { queryClient } from '@/lib/query-client'
import { ThemeProvider, useTheme } from '@/lib/theme'
import '@/styles/globals.css'

function AppToaster() {
  const { resolved } = useTheme()
  return <Toaster richColors position="top-center" theme={resolved} />
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <BrowserRouter>
          <App />
        </BrowserRouter>
        <AppToaster />
      </QueryClientProvider>
    </ThemeProvider>
  </StrictMode>,
)