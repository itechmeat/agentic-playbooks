import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import ProfileList from './ProfileList.svelte'

describe('ProfileList', () => {
  it('SSR-renders the project filter selector', () => {
    const { body } = render(ProfileList, { props: {} })
    expect(body).toContain('Project filter')
  })
})
