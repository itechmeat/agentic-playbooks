import { describe, expect, it } from 'vitest'
import { formatDuration, shortModel } from './format'

describe('formatDuration', () => {
  it('rounds milliseconds below a second', () => {
    expect(formatDuration(12.6)).toBe('13 ms')
    expect(formatDuration(0)).toBe('0 ms')
  })

  it('floors seconds, minutes and hours', () => {
    expect(formatDuration(42_900)).toBe('42s')
    expect(formatDuration(365_000)).toBe('6m 05s')
    expect(formatDuration(41 * 60_000)).toBe('41m 00s')
    expect(formatDuration(6_720_000)).toBe('1h 52m')
    expect(formatDuration(50 * 3_600_000)).toBe('2d 2h')
  })

  it('reads a negative span as zero and a non-finite one as a dash', () => {
    expect(formatDuration(-5)).toBe('0 ms')
    expect(formatDuration(Number.NaN)).toBe('-')
  })
})

describe('shortModel', () => {
  it('shortens claude ids and leaves the rest as written', () => {
    expect(shortModel('claude-opus-5-5')).toBe('opus 5.5')
    expect(shortModel('claude-sonnet-5')).toBe('sonnet 5')
    expect(shortModel('GLM-5.3-Flash')).toBe('GLM-5.3-Flash')
    expect(shortModel('opus')).toBe('opus')
  })
})
