import { create } from 'zustand'

interface UiState {
  activeSessionId: string | null
  setActiveSession: (id: string) => void
  clearActiveSession: () => void
  mobileMenuOpen: boolean
  setMobileMenuOpen: (open: boolean) => void
}

export const useUiStore = create<UiState>()((set) => ({
  activeSessionId: null,
  setActiveSession: (id) => set({ activeSessionId: id }),
  clearActiveSession: () => set({ activeSessionId: null }),
  mobileMenuOpen: false,
  setMobileMenuOpen: (open) => set({ mobileMenuOpen: open }),
}))