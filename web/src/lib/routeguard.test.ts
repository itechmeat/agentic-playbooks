import { describe, expect, it } from 'vitest'
import { sameRoute } from './routeguard'

describe('sameRoute', () => {
  it('holds until the page moves to another route, then never again', () => {
    let token = 3
    const still = sameRoute(() => token)
    expect(still()).toBe(true)
    token += 1
    expect(still()).toBe(false)
    // A newer request's guard holds for the new route only.
    const next = sameRoute(() => token)
    expect(next()).toBe(true)
    expect(still()).toBe(false)
  })
})
