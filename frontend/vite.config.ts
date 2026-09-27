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
      // Generated App Protocol SDK (single typed boundary to the Rust facade).
      '@cool-sdk': path.resolve(__dirname, '../sdk/typescript/src'),
    },
  },
  server: {
    // Bind IPv4 explicitly — Vite defaults to IPv6-only (::1) on some
    // Windows setups, which makes `curl 127.0.0.1:5173` silently fail.
    host: "127.0.0.1",
    port: 5173,
    // Proxy /api to `cool serve` so the SPA can call same-origin URLs.
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8000",
        changeOrigin: true,
      },
    },
  },
})
