import path from 'node:path'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// 后端默认以 HTTPS (自签证书) 提供服务，见 backend/scripts/gen-cert.ps1。
// secure:false —— 开发代理跳过对自签名证书的校验。
const API_TARGET = 'https://127.0.0.1:3000'

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
      '/chat': { target: API_TARGET, secure: false },
      '/sessions': { target: API_TARGET, secure: false },
      '/memories': { target: API_TARGET, secure: false },
      '/memory': { target: API_TARGET, secure: false },
      '/search': { target: API_TARGET, secure: false },
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