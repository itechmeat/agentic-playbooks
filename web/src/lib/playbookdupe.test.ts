import { describe, expect, it } from 'vitest'
import { suggestDuplicateId } from './playbookdupe'

describe('suggestDuplicateId', () => {
  it('appends -copy to source id', () => {
    expect(suggestDuplicateId('demo')).toBe('demo-copy')
    expect(suggestDuplicateId('implement-task')).toBe('implement-task-copy')
  })
})
