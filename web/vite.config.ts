import path from 'node:path'
import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { DEFAULT_PORT } from './src/lib/consts.gen.ts'

/**
 * Fonts are always emitted as files under `assets/`, never inlined as
 * `data:` URIs: the dashboard's CSP admits fonts from the server only
 * (`font-src 'self'`), so an inlined font is blocked and logged. Everything
 * else keeps Vite's default limit.
 */
export function inlineAsset(file: string): false | undefined {
  return /\.(woff2?|ttf|otf|eot)$/i.test(file) ? false : undefined
}

export default defineConfig({
  // Under vitest, drop the tailwind plugin: its startup scan fetches a
  // component's virtual style module before the svelte plugin has compiled
  // it, the load falls back to the raw file, and the CSS parser fails on the
  // script text (CssSyntaxError on a comment that holds an apostrophe). No
  // test imports `app.css`, so tests need plain CSS passthrough only.
  plugins: [...(process.env.VITEST ? [] : [tailwindcss()]), svelte()],
  build: {
    assetsInlineLimit: inlineAsset,
  },
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
