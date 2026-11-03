import { describe, expect, it } from 'vitest'
import { effectTags } from './effecttags'

describe('effectTags', () => {
  it('marks what only the reading of a script that declared its own effects puts there as assumed', () => {
    expect(effectTags(['fs_read', 'fs_write', 'network', 'external'], ['network', 'external'])).toEqual([
      { effect: 'fs_read', assumed: false },
      { effect: 'fs_write', assumed: false },
      { effect: 'network', assumed: true },
      { effect: 'external', assumed: true },
    ])
  })

  it('marks nothing when the server names nothing assumed, as an older one does', () => {
    expect(effectTags(['fs_write', 'fs_write', 'network'])).toEqual([
      { effect: 'fs_write', assumed: false },
      { effect: 'network', assumed: false },
    ])
  })
})
