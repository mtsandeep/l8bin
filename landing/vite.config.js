import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vite'
import { resolve } from 'path'

export default defineConfig({
  plugins: [tailwindcss()],
  build: {
    rollupOptions: {
      input: {
        index: resolve(__dirname, 'index.html'),
        quickstart: resolve(__dirname, 'quickstart.html'),
        live: resolve(__dirname, 'live/index.html'),
        'logo-demo': resolve(__dirname, 'logo.html'),
      },
    },
  },
})
