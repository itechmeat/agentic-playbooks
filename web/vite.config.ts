import path from 'node:path'
import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { DEFAULT_PORT } from './src/lib/consts.gen.ts'

export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  resolve: {
    alias: {
      $lib: path.resolve('./src/lib'),
    },
    // Under vitest, resolve `svelte` to its browser build so a test in a DOM
    // environment (`@vitest-environment happy-dom`) can `mount` a component.
    // `svelte/server` is an explicit subpath, so the SSR tests still render.
    ...(process.env.VITEST ? { conditions: ['browser'] } : {}),
  },
  server: {
    proxy: {
      '/api': {
        target: `http://127.0.0.1:${DEFAULT_PORT}`,
        ws: true,
      },
    },
  },
})
