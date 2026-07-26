import { describe, it, expect } from 'vitest'
import { NODE_FIELDS } from './nodefields'

describe('NODE_FIELDS', () => {
  it('gives every field a written label, not the raw schema key', () => {
    for (const [key, info] of Object.entries(NODE_FIELDS)) {
      expect(info.label, key).not.toContain('_')
      expect(info.label[0], key).toBe(info.label[0]?.toUpperCase())
    }
  })

  it('explains every field in prose rather than restating the label', () => {
    for (const [key, info] of Object.entries(NODE_FIELDS)) {
      expect(info.hint.length, key).toBeGreaterThan(40)
      expect(info.hint.trim().endsWith('.'), key).toBe(true)
    }
  })
})
