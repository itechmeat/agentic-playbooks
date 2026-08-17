import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import Login from './Login.svelte'

describe('Login', () => {
  it('SSR-renders the key field and the sign-in action', () => {
    const { body } = render(Login, { props: {} })
    expect(body).toContain('Authorization key')
    expect(body).toContain('Sign in')
    expect(body).toContain('type="password"')
  })

  it('explains where the key comes from', () => {
    const { body } = render(Login, { props: {} })
    expect(body).toContain('apb server key issue')
  })
})
