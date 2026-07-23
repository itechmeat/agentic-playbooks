import { describe, expect, it } from 'vitest'
import { waitingKindText } from './progress'

describe('waitingKindText', () => {
  it('maps each waiting_kind to distinct badge copy', () => {
    expect(waitingKindText('human_review')).toBe('waiting for decision')
    expect(waitingKindText('wait')).toBe('waiting for event')
    expect(waitingKindText('question')).toBe('waiting for answer')
    expect(waitingKindText('supervisor')).toBe('waiting for supervisor')
  })

  it('falls back to a generic waiting label when kind is null', () => {
    expect(waitingKindText(null)).toBe('waiting')
  })
})
