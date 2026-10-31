// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { slowRefresh } from './ws'

describe('slowRefresh', () => {
  let visibility: DocumentVisibilityState = 'visible'
  beforeEach(() => {
    vi.useFakeTimers()
    visibility = 'visible'
    vi.spyOn(document, 'visibilityState', 'get').mockImplementation(() => visibility)
  })
  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it('runs the first request at once and collapses the rest of the window into one call', () => {
    const fn = vi.fn()
    const r = slowRefresh(fn, 10_000)
    r.request()
    expect(fn).toHaveBeenCalledTimes(1)
    for (let i = 0; i < 20; i++) r.request()
    expect(fn).toHaveBeenCalledTimes(1)
    vi.advanceTimersByTime(10_000)
    expect(fn).toHaveBeenCalledTimes(2)
    r.dispose()
  })

  it('runs the first request even in a hidden tab, so a background page still gets its data', () => {
    const fn = vi.fn()
    const r = slowRefresh(fn, 10_000)
    visibility = 'hidden'
    r.request()
    expect(fn).toHaveBeenCalledTimes(1)
    r.dispose()
  })

  it('waits while the tab is hidden after the first call and runs once it is shown', () => {
    const fn = vi.fn()
    const r = slowRefresh(fn, 10_000)
    r.request()
    visibility = 'hidden'
    vi.advanceTimersByTime(10_000)
    r.request()
    expect(fn).toHaveBeenCalledTimes(1)
    visibility = 'visible'
    document.dispatchEvent(new Event('visibilitychange'))
    expect(fn).toHaveBeenCalledTimes(2)
    r.dispose()
  })

  it('drops a pending call on dispose', () => {
    const fn = vi.fn()
    const r = slowRefresh(fn, 10_000)
    r.request()
    r.request()
    r.dispose()
    vi.advanceTimersByTime(20_000)
    expect(fn).toHaveBeenCalledTimes(1)
  })
})
