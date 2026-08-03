import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'path'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
      // 仓库级 i18n 目录（frontend 的上三级），翻译 JSON 由桌面端/后端共享
      '@i18n': path.resolve(__dirname, '../../../i18n'),
    },
  },
  build: {
    outDir: '../static',
    emptyOutDir: true,
  },
  server: {
    port: 5173,
    fs: {
      // 允许 dev server 读取 frontend/ 之外的仓库级 i18n JSON
      allow: [path.resolve(__dirname, '../../..')],
    },
    proxy: {
      '/api': 'http://localhost:9800',
      '/ws': {
        target: 'ws://localhost:9800',
        ws: true,
      },
    },
  },
})
