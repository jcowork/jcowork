import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    host: '0.0.0.0',
    allowedHosts: ['home.top.jx.cn'],
    proxy: {
      '/api/ws': {
        target: process.env.API_TARGET || 'ws://localhost:3000',
        changeOrigin: true,
        ws: true,
      },
      // Voice config is plain HTTP — must match before the WS rule below.
      '/api/voice/config': {
        target: process.env.API_TARGET || 'http://localhost:3000',
        changeOrigin: true,
      },
      '/api/voice': {
        target: process.env.API_TARGET || 'ws://localhost:3000',
        changeOrigin: true,
        ws: true,
      },
      '/api': {
        target: process.env.API_TARGET || 'http://localhost:3000',
        changeOrigin: true,
      },
    },
  },
})
