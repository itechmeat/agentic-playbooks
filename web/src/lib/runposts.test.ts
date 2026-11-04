import { describe, expect, it, vi } from 'vitest'

const { postReview } = vi.hoisted(() => ({ postReview: vi.fn() }))
vi.mock('./api', async () => {
  const http = await import('./api/http')
  return { ApiError: http.ApiError, postAnswer: vi.fn(), postReview, resolveIntent: vi.fn() }
})

import { ApiError } from './api/http'
import { PASSKEYS_MESSAGE } from './passkeys'
import { postDecision } from './runposts'

const target = { runId: 'r1', node: 'gate', request: 7 }

describe('postDecision with a passkey enrolled', () => {
  it('posts plainly when the first post succeeds', async () => {
    postReview.mockResolvedValueOnce({ posted_seq: 1 })
    await expect(postDecision(target, 'approve', 'w')).resolves.toEqual({ posted_seq: 1 })
    expect(postReview).toHaveBeenCalledTimes(1)
  })

  it('rethrows a refusal that does not name a passkey', async () => {
    const err = new ApiError('person only', 403)
    postReview.mockRejectedValueOnce(err)
    await expect(postDecision(target, 'approve', 'w')).rejects.toBe(err)
  })

  it('runs the assertion and posts again with the token on a passkey refusal', async () => {
    postReview
      .mockRejectedValueOnce(new ApiError('this gate needs a passkey: use the dashboard', 403))
      .mockResolvedValueOnce({ posted_seq: 2 })
    const deps = {
      secureContext: () => true,
      assertChallenge: vi.fn(async () => ({ challenge: 'A'.repeat(43) })),
      getAssertion: vi.fn(async () => ({
        id: 'cred-1',
        clientDataJSON: 'CDA',
        authenticatorData: 'AUTHDATA',
        signature: 'SIG',
      })),
      verifyAssertion: vi.fn(async () => ({ token: 'TOKEN' })),
    }
    await expect(postDecision(target, 'approve', 'w', '', deps)).resolves.toEqual({ posted_seq: 2 })
    expect(deps.assertChallenge).toHaveBeenCalledWith({
      decision: { run: 'r1', node: 'gate', decision: 'approve', request: 7, nonce: null },
    })
    expect(postReview).toHaveBeenLastCalledWith('r1', 'gate', 'approve', '', 'w', undefined, 7, 'TOKEN')
  })

  it('shows PASSKEYS_MESSAGE without a ceremony when not a secure context', async () => {
    postReview.mockRejectedValueOnce(new ApiError('this gate needs a passkey', 403))
    const deps = {
      secureContext: () => false,
      assertChallenge: vi.fn(async () => ({ challenge: 'x' })),
      getAssertion: vi.fn(async () => ({ id: 'c', clientDataJSON: 'C', authenticatorData: 'A', signature: 'S' })),
      verifyAssertion: vi.fn(async () => ({ token: 'TOKEN' })),
    }
    await expect(postDecision(target, 'approve', 'w', '', deps)).rejects.toThrow(PASSKEYS_MESSAGE)
    expect(deps.getAssertion).not.toHaveBeenCalled()
  })
})
