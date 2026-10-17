import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vite'
import { resolve } from 'path'

export default defineConfig({
  plugins: [
    tailwindcss(),
    // Serve text/plain as UTF-8 (llms.txt) — vite omits the charset and the
    // browser falls back to a legacy codepage, mojibake-ing the em-dashes.
    {
      name: 'plain-text-charset',
      configureServer(server) {
        server.middlewares.use((_req, res, next) => {
          const set = res.setHeader.bind(res)
          res.setHeader = (name, value) => {
            if (String(name).toLowerCase() === 'content-type'
                && String(value).startsWith('text/plain')
                && !String(value).includes('charset')) {
              value += '; charset=utf-8'
            }
            return set(name, value)
          }
          next()
        })
      },
    },
  ],
  build: {
    rollupOptions: {
      input: {
        index: resolve(__dirname, 'index.html'),
        quickstart: resolve(__dirname, 'quickstart.html'),
        agents: resolve(__dirname, 'agents.html'),
        live: resolve(__dirname, 'live/index.html'),
        docs: resolve(__dirname, 'docs/index.html'),
      },
    },
  },
})
