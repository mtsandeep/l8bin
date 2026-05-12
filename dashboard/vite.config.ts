import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 5088,
    proxy: {
      '/health': { target: 'http://localhost:5080', changeOrigin: true },
      '/projects': { target: 'http://localhost:5080', changeOrigin: true },
      '/deploy': { target: 'http://localhost:5080', changeOrigin: true },
      '/deploy-tokens': { target: 'http://localhost:5080', changeOrigin: true },
      '/images': { target: 'http://localhost:5080', changeOrigin: true },
      '/auth': { target: 'http://localhost:5080', changeOrigin: true },
      '/settings': { target: 'http://localhost:5080', changeOrigin: true },
      '/nodes': { target: 'http://localhost:5080', changeOrigin: true },
      '/system': { target: 'http://localhost:5080', changeOrigin: true },
      '/scan': { target: 'http://localhost:5080', changeOrigin: true },
      '/status': { target: 'http://localhost:5080', changeOrigin: true },
    },
  },
});
