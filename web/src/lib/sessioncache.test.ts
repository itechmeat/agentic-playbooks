import { describe, it, expect, vi } from 'vitest'
import { cachedJson, type StorageLike } from './sessioncache'

function memoryStorage(initial: Record<string, string> = {}): StorageLike & {
  data: Record<string, string>
} {
  const data = { ...initial }
  return {
    data,
    getItem(k: string) {
      return Object.prototype.hasOwnProperty.call(data, k) ? data[k]! : null
    },
    setItem(k: string, v: string) {
      data[k] = v
    },
  }
}

describe('cachedJson', () => {
  it('returns cached data without calling the fetcher on a fresh hit', async () => {
    const storage = memoryStorage({
      'k': JSON.stringify({ at: Date.now(), data: 'cached' }),
    })
    const fetcher = vi.fn().mockResolvedValue('fresh')

    const result = await cachedJson('k', 60_000, fetcher, storage)

    expect(result).toBe('cached')
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('refetches and overwrites an expired entry', async () => {
    const storage = memoryStorage({
      'k': JSON.stringify({ at: Date.now() - 120_000, data: 'stale' }),
    })
    const fetcher = vi.fn().mockResolvedValue('fresh')

    const result = await cachedJson('k', 60_000, fetcher, storage)

    expect(result).toBe('fresh')
    expect(fetcher).toHaveBeenCalledTimes(1)
    expect(JSON.parse(storage.getItem('k')!).data).toBe('fresh')
  })

  it('treats an entry exactly at the TTL boundary as stale and refetches', async () => {
    // readFresh() stales out at `now - at >= ttlMs`, so an entry aged exactly
    // `ttlMs` is stale, not fresh, by one tick of the inequality.
    const storage = memoryStorage({
      'k': JSON.stringify({ at: Date.now() - 60_000, data: 'stale' }),
    })
    const fetcher = vi.fn().mockResolvedValue('fresh')

    const result = await cachedJson('k', 60_000, fetcher, storage)

    expect(result).toBe('fresh')
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('refetches an unparsable entry', async () => {
    const storage = memoryStorage({ 'k': 'not json' })
    const fetcher = vi.fn().mockResolvedValue('fresh')

    const result = await cachedJson('k', 60_000, fetcher, storage)

    expect(result).toBe('fresh')
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('shares one fetcher invocation across concurrent calls for the same key', async () => {
    const storage = memoryStorage()
    let resolveFetch: (v: string) => void
    const fetcher = vi.fn(
      () =>
        new Promise<string>((resolve) => {
          resolveFetch = resolve
        }),
    )

    const p1 = cachedJson('k', 60_000, fetcher, storage)
    const p2 = cachedJson('k', 60_000, fetcher, storage)
    resolveFetch!('fresh')

    const [r1, r2] = await Promise.all([p1, p2])

    expect(r1).toBe('fresh')
    expect(r2).toBe('fresh')
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('leaves no cache entry on fetcher failure, and a later call retries', async () => {
    const storage = memoryStorage()
    const fetcher = vi
      .fn()
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce('fresh')

    await expect(cachedJson('k', 60_000, fetcher, storage)).rejects.toThrow('boom')
    expect(storage.getItem('k')).toBeNull()

    const result = await cachedJson('k', 60_000, fetcher, storage)

    expect(result).toBe('fresh')
    expect(fetcher).toHaveBeenCalledTimes(2)
  })

  it('returns the fetched data even when storage.setItem throws', async () => {
    const storage: StorageLike = {
      getItem: () => null,
      setItem: () => {
        throw new Error('quota exceeded')
      },
    }
    const fetcher = vi.fn().mockResolvedValue('fresh')

    const result = await cachedJson('k', 60_000, fetcher, storage)

    expect(result).toBe('fresh')
  })
})
