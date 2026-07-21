import path from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    // Bind IPv4 explicitly — Vite defaults to IPv6-only (::1) on some
    // Windows setups, which makes `curl 127.0.0.1:5173` silently fail.
    host: "127.0.0.1",
    port: 5173,
    // Proxy /api and /ws to the backend so the SPA can call same-origin URLs.
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8000",
        changeOrigin: true,
      },
      "/ws": {
        target: "ws://127.0.0.1:8000",
        ws: true,
        changeOrigin: true,
      },
    },
  },
})
