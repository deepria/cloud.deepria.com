import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [vue()],
  server: {
    host: '127.0.0.1',
    strictPort: true,
    proxy: {
      '/api': process.env.CLOUD_API_PROXY ?? 'http://127.0.0.1:8080',
      '/uploads/': process.env.CLOUD_API_PROXY ?? 'http://127.0.0.1:8080',
    },
  },
})
