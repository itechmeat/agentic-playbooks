// Covers `authState()`: the one export in this module that uses runes
// ($state/$effect), so it needs the `.svelte.` infix in the filename for
// vite-plugin-svelte to compile runes here too (see
// https://svelte.dev/docs/svelte/testing).
//
// This project's Vitest setup has no browser/jsdom environment - every other
// component test in the repo renders through `svelte/server` (see
// `ProfileList.test.ts`), so Vite compiles Svelte modules here with
// `generate: 'server'`. Under that target, `$effect` bodies are stripped
// entirely (SSR has no reactivity loop to run them on), so the store
// subscription inside `authState()` never actually fires in this harness and
// a live-update assertion cannot be exercised here - that would need a real
// browser/jsdom test environment, which this fix does not add (bigger change
// than warranted; every other test in the repo already accepts this SSR-only
// ceiling). What IS meaningful and harness-safe: the call does not throw
// under either target, returns the documented optimistic default, and hands
// each caller its own independent reactive object rather than a shared
// module-level singleton.
import { describe, expect, it } from 'vitest'
import { authState } from './auth.svelte'

describe('authState', () => {
  it('starts at the optimistic default', () => {
    expect(authState()).toEqual({ required: false, authenticated: true, checked: false })
  })

  it('returns a fresh object per call, not a shared singleton', () => {
    expect(authState()).not.toBe(authState())
  })
})
