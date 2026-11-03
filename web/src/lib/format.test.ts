import { describe, expect, it } from 'vitest'
import { formatDuration, shortModel, shownNumber } from './format'

describe('formatDuration', () => {
  it('rounds milliseconds below a second', () => {
    expect(formatDuration(12.6)).toBe('13 ms')
    expect(formatDuration(0)).toBe('0 ms')
    expect(formatDuration(999.7)).toBe('1s')
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

  it('drops the date of a dated claude id', () => {
    expect(shortModel('claude-sonnet-4-20250514')).toBe('sonnet 4')
    expect(shortModel('claude-opus-4-1-20250805')).toBe('opus 4.1')
  })

  it('leaves lowercase ids of other providers as written', () => {
    expect(shortModel('gpt-5')).toBe('gpt-5')
    expect(shortModel('gpt-4-0613')).toBe('gpt-4-0613')
    expect(shortModel('glm-4.6')).toBe('glm-4.6')
  })
})

describe('shownNumber', () => {
  it('shows a recorded 32-bit float as a person would write it, never as float noise', () => {
    expect(shownNumber(0.8999999761581421)).toBe('0.9')
    expect(shownNumber(0.6200000047683716)).toBe('0.62')
    expect(shownNumber(1.4999999701976776)).toBe('1.5')
    expect(shownNumber(3)).toBe('3')
    expect(shownNumber(0.123456789)).toBe('0.123457')
  })
})
