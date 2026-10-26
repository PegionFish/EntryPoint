/// <reference types="vitest/config" />
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
  // T-28a：vitest 单测（`npm test` = vitest run）。
  // jsdom 环境：use-canvas-history 等 React hook 测试（@testing-library/react）
  // 需要 DOM；纯函数测试（spec-io round-trip）在 jsdom 下同样工作。
  // test 段仅被 vitest 消费，`npm run build` 产物不受影响（devDependency 不入 bundle）。
  test: {
    environment: 'jsdom',
    include: ['src/**/*.{test,spec}.{ts,tsx}'],
  },
})
