import path from 'node:path'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src'),
    },
  },
  server: {
    host: true,
    port: 5173,
    proxy: {
      '/chat': 'http://127.0.0.1:3000',
      '/sessions': 'http://127.0.0.1:3000',
      '/memories': 'http://127.0.0.1:3000',
      '/memory': 'http://127.0.0.1:3000',
      '/search': 'http://127.0.0.1:3000',
      '/entities': 'http://127.0.0.1:3000',
      '/relations': 'http://127.0.0.1:3000',
    },
  },
  build: {
    chunkSizeWarningLimit: 1200,
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            {
              name: 'assistant-ui',
              test: /[\\/]node_modules[\\/](@assistant-ui|react-markdown|remark[\\/-]|rehype[\\/-]|unified|micromark[\\/-]|mdast[\\/-]|hast[\\/-])[\\/]/,
            },
          ],
        },
      },
    },
  },
})