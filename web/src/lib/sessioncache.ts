// A small TTL cache over sessionStorage for slow, rarely-changing GET
// lookups (agents/models detection). Concurrent calls for the same key share
// one in-flight fetch; a failed fetch never poisons the cache.

/** Minimal storage surface used by the cache (real sessionStorage or a test double). */
export type StorageLike = {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
}

interface Entry<T> {
  at: number
  data: T
}

function guardedSessionStorage(): StorageLike | undefined {
  if (typeof sessionStorage !== 'undefined') return sessionStorage
  return undefined
}

// Shared across all callers in this module instance, keyed by cache key, so
// concurrent calls for the same key await one underlying fetch.
const inFlight = new Map<string, Promise<unknown>>()

function readFresh<T>(backend: StorageLike | undefined, key: string, ttlMs: number): T | undefined {
  if (!backend) return undefined
  try {
    const raw = backend.getItem(key)
    if (raw == null) return undefined
    const entry = JSON.parse(raw) as Entry<T>
    if (typeof entry.at !== 'number') return undefined
    if (Date.now() - entry.at >= ttlMs) return undefined
    return entry.data
  } catch {
    // Unparsable or otherwise malformed entry: treat as a miss.
    return undefined
  }
}

function writeEntry<T>(backend: StorageLike | undefined, key: string, data: T): void {
  if (!backend) return
  try {
    backend.setItem(key, JSON.stringify({ at: Date.now(), data } satisfies Entry<T>))
  } catch {
    // Quota exceeded or storage disabled: the fetch result is still returned
    // to the caller, it just won't be cached for next time.
  }
}

/**
 * Fetch-through cache backed by sessionStorage (or an injected `storage`).
 * Fresh entries (`now - at < ttlMs`) are returned without calling `fetcher`.
 * Concurrent calls for the same `key` share one in-flight `fetcher()` call.
 * A failed fetch leaves no cache entry, so the next call retries.
 *
 * Contract: `T` must be JSON-serializable, and `fetcher` must not legitimately
 * resolve to `undefined` - `undefined` is the internal sentinel for "no fresh
 * entry" (missing, stale, or corrupt), so a fetcher that can genuinely resolve
 * to `undefined` would never register as a cache hit and would refetch every
 * call.
 */
export function cachedJson<T>(
  key: string,
  ttlMs: number,
  fetcher: () => Promise<T>,
  storage?: StorageLike,
): Promise<T> {
  const backend = storage ?? guardedSessionStorage()

  const fresh = readFresh<T>(backend, key, ttlMs)
  if (fresh !== undefined) return Promise.resolve(fresh)

  const pending = inFlight.get(key) as Promise<T> | undefined
  if (pending) return pending

  const request = fetcher()
    .then((data) => {
      writeEntry(backend, key, data)
      return data
    })
    .finally(() => {
      inFlight.delete(key)
    })
  inFlight.set(key, request)
  return request
}
