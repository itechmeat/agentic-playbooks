import { describe, expect, it } from 'vitest'
import { attemptOutputNote } from './outputloss'

describe('attemptOutputNote', () => {
  it('names each cut stream and an unreadable final result', () => {
    expect(
      attemptOutputNote([{ stream: 'stdout', kept_bytes: 5120, dropped_bytes_at_least: 14980 }], true),
    ).toBe('output cut: stdout kept 5120 bytes, at least 14980 dropped; final result not read whole')
    expect(attemptOutputNote([], true)).toBe('output cut: final result not read whole')
  })

  it('is absent for an attempt that lost nothing or an older event', () => {
    expect(attemptOutputNote(undefined, undefined)).toBeUndefined()
    expect(attemptOutputNote([], false)).toBeUndefined()
    expect(attemptOutputNote([{ stream: 'stdout' }], false)).toBeUndefined()
  })
})
