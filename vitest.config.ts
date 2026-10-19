import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url))
    }
  },
  test: {
    environment: 'happy-dom',
    include: ['tests/**/*.test.ts'],
    // 本地存储层测试彼此独立，串行跑最稳（共享盘上并发 fork 偶发抖动）
    pool: 'forks'
  }
})
