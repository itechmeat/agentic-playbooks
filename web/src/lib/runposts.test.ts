import { describe, expect, it, vi } from 'vitest'

const { postReview } = vi.hoisted(() => ({ postReview: vi.fn() }))
vi.mock('./api', async () => {
  const http = await import('./api/http')
  return { ApiError: http.ApiError, postAnswer: vi.fn(), postReview, resolveIntent: vi.fn() }
})

import { ApiError } from './api/http'
import { PASSKEYS_MESSAGE } from './passkeys'
import { b64u, postDecision } from './runposts'

describe('the browser wire format', () => {
  it('encodes the authenticator fields as base64url', () => {
    // 0xFB 0xFF 0xEF would be "+/+8A" with padding in standard base64;
    // base64url is "-__vAA" with none, exactly what the routes decode.
    const bytes = new Uint8Array([0xfb, 0xff, 0xef, 0x00])
    const encoded = b64u(bytes.buffer)
    expect(encoded).toBe('-__vAA')
    expect(encoded).not.toMatch(/[+/=]/)
  })
})

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
    // The verify step gets the assertion the authenticator returned.
    expect(deps.verifyAssertion).toHaveBeenCalledWith(
      { decision: { run: 'r1', node: 'gate', decision: 'approve', request: 7, nonce: null } },
      'A'.repeat(43),
      { id: 'cred-1', clientDataJSON: 'CDA', authenticatorData: 'AUTHDATA', signature: 'SIG' },
    )
    expect(postReview).toHaveBeenLastCalledWith('r1', 'gate', 'approve', '', 'w', undefined, 7, 'TOKEN', {
      assertionId: 'cred-1',
      assertionClientDataJSON: 'CDA',
      assertionAuthenticatorData: 'AUTHDATA',
      assertionSignature: 'SIG',
    })
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
