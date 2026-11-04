import { describe, expect, it, vi } from 'vitest'
import {
  PASSKEYS_MESSAGE,
  challengeBytes,
  decisionPurpose,
  enrollNewPurpose,
  removePurpose,
  runDecisionWithPasskey,
  runEnroll,
  secureContextOk,
  type AssertionResult,
  type DecisionDeps,
  type EnrollDeps,
  type PostOutcome,
  type ReviewBody,
} from './passkeys'

// Every dep is a vi.fn so each test asserts exactly what was called and with
// what; no real fetch and no real navigator anywhere.

const assertion = (): AssertionResult => ({
  id: 'cred-1',
  clientDataJSON: 'eyJ0eXBlIjoid2ViYXV0aG4uZ2V0In0',
  authenticatorData: 'AUTHDATA',
  signature: 'SIG',
})

function decisionDeps(overrides: Partial<DecisionDeps> = {}): DecisionDeps {
  return {
    postReview: vi.fn(async (): Promise<PostOutcome> => ({ ok: true })),
    assertChallenge: vi.fn(async () => ({ challenge: 'CHALLENGE' })),
    verifyAssertion: vi.fn(async () => ({ token: 'TOKEN' })),
    getAssertion: vi.fn(async () => assertion()),
    secureContext: vi.fn(() => true),
    ...overrides,
  }
}

function enrollDeps(overrides: Partial<EnrollDeps> = {}): EnrollDeps {
  return {
    startEnroll: vi.fn(async () => ({
      challenge: 'ENROLL-CHALLENGE',
      rpId: 'example.com',
      userVerification: 'required',
      attestation: 'none',
      algorithms: [-7, -257],
    })),
    finishEnroll: vi.fn(async () => ({ label: 'laptop', created_at: '2026-01-01T00:00:00Z', id_prefix: 'abcd' })),
    createCredential: vi.fn(async () => ({
      id: 'cred-new',
      clientDataJSON: 'CDA',
      attestationObject: 'ATTOBJ',
    })),
    secureContext: vi.fn(() => true),
    ...overrides,
  }
}

const body: ReviewBody = { node: 'gate', decision: 'approve' }

describe('secure context', () => {
  it('follows window.isSecureContext', () => {
    vi.stubGlobal('window', { isSecureContext: true })
    expect(secureContextOk()).toBe(true)
    vi.stubGlobal('window', { isSecureContext: false })
    expect(secureContextOk()).toBe(false)
    vi.unstubAllGlobals()
  })

  it('falls back to localhost hostnames when isSecureContext is absent', () => {
    vi.stubGlobal('window', undefined)
    vi.stubGlobal('location', { hostname: 'localhost' })
    expect(secureContextOk()).toBe(true)
    vi.stubGlobal('location', { hostname: 'dashboard.localhost' })
    expect(secureContextOk()).toBe(true)
    vi.stubGlobal('location', { hostname: '127.0.0.1' })
    expect(secureContextOk()).toBe(true)
    vi.stubGlobal('location', { hostname: '[::1]' })
    expect(secureContextOk()).toBe(true)
    vi.stubGlobal('location', { hostname: 'example.com' })
    expect(secureContextOk()).toBe(false)
    vi.unstubAllGlobals()
  })

  it('is false with no window and no location', () => {
    vi.stubGlobal('window', undefined)
    vi.stubGlobal('location', undefined)
    expect(secureContextOk()).toBe(false)
    vi.unstubAllGlobals()
  })
})

describe('challengeBytes', () => {
  it('decodes the base64url challenge the server issued', () => {
    // 32 zero bytes encode to 43 unpadded base64url chars.
    expect(Array.from(challengeBytes('A'.repeat(43)))).toEqual(new Array(32).fill(0))
    // The ASCII bytes of the challenge string are not the challenge.
    expect(Array.from(challengeBytes('AQID'))).toEqual([1, 2, 3])
  })
})

describe('assert purposes', () => {
  it('builds the three shapes the server routes read', () => {
    expect(decisionPurpose('r1', 'gate', 'approve', 7, 'n')).toEqual({
      decision: { run: 'r1', node: 'gate', decision: 'approve', request: 7, nonce: 'n' },
    })
    expect(decisionPurpose('r1', 'gate', 'approve')).toEqual({
      decision: { run: 'r1', node: 'gate', decision: 'approve', request: null, nonce: null },
    })
    expect(enrollNewPurpose()).toEqual({ enroll_new: {} })
    expect(removePurpose('laptop')).toEqual({ remove: 'laptop' })
  })
})

describe('runDecisionWithPasskey', () => {
  it('returns without a ceremony when the review succeeds', async () => {
    const deps = decisionDeps()
    const got = await runDecisionWithPasskey(deps, 'r1', body)
    expect(got).toEqual({ ok: true })
    expect(deps.assertChallenge).not.toHaveBeenCalled()
    expect(deps.getAssertion).not.toHaveBeenCalled()
    expect(deps.verifyAssertion).not.toHaveBeenCalled()
    expect(deps.postReview).toHaveBeenCalledTimes(1)
  })

  it('runs the ceremony on a 403 naming a passkey and posts again with the token', async () => {
    const posts: ReviewBody[] = []
    const deps = decisionDeps({
      postReview: vi.fn(async (_id: string, b: ReviewBody): Promise<PostOutcome> => {
        posts.push(b)
        return b.passkey_token ? { ok: true } : { ok: false, status: 403, message: 'gate needs a passkey' }
      }),
    })
    const got = await runDecisionWithPasskey(deps, 'r1', {
      node: 'gate',
      decision: 'approve',
      request: 3,
      nonce: 'plan-n',
    })
    expect(got).toEqual({ ok: true })
    expect(deps.assertChallenge).toHaveBeenCalledWith({
      decision: { run: 'r1', node: 'gate', decision: 'approve', request: 3, nonce: 'plan-n' },
    })
    expect(deps.getAssertion).toHaveBeenCalledWith('CHALLENGE')
    expect(deps.verifyAssertion).toHaveBeenCalledWith(
      { decision: { run: 'r1', node: 'gate', decision: 'approve', request: 3, nonce: 'plan-n' } },
      'CHALLENGE',
    )
    expect(posts[1]?.passkey_token).toBe('TOKEN')
  })

  it('surfaces a 403 without the passkey message and never starts a ceremony', async () => {
    const deps = decisionDeps({
      postReview: vi.fn(async () => ({ ok: false, status: 403, message: 'person only' })),
    })
    const got = await runDecisionWithPasskey(deps, 'r1', body)
    expect(got).toEqual({ ok: false, status: 403, message: 'person only' })
    expect(deps.getAssertion).not.toHaveBeenCalled()
    expect(deps.postReview).toHaveBeenCalledTimes(1)
  })

  it('returns PASSKEYS_MESSAGE without a ceremony when not a secure context', async () => {
    const deps = decisionDeps({
      postReview: vi.fn(async () => ({ ok: false, status: 403, message: 'gate needs a passkey' })),
      secureContext: vi.fn(() => false),
    })
    const got = await runDecisionWithPasskey(deps, 'r1', body)
    expect(got).toEqual({ ok: false, status: 403, message: PASSKEYS_MESSAGE })
    expect(deps.getAssertion).not.toHaveBeenCalled()
    expect(deps.postReview).toHaveBeenCalledTimes(1)
  })

  it('surfaces a failing verify without posting again', async () => {
    const deps = decisionDeps({
      postReview: vi.fn(async () => ({ ok: false, status: 403, message: 'review needs a passkey' })),
      verifyAssertion: vi.fn(async () => {
        throw new Error('assertion refused')
      }),
    })
    await expect(runDecisionWithPasskey(deps, 'r1', body)).rejects.toThrow('assertion refused')
    expect(deps.postReview).toHaveBeenCalledTimes(1)
  })
})

describe('runEnroll', () => {
  it('starts with the one-time code when this is the first passkey', async () => {
    const deps = enrollDeps()
    const got = await runEnroll(deps, { label: 'laptop', code: '123456' })
    expect(got).toEqual({ ok: true, entry: { label: 'laptop', created_at: '2026-01-01T00:00:00Z', id_prefix: 'abcd' } })
    expect(deps.startEnroll).toHaveBeenCalledWith({ code: '123456' })
    expect(deps.createCredential).toHaveBeenCalledWith({
      challenge: 'ENROLL-CHALLENGE',
      rpId: 'example.com',
      userVerification: 'required',
      attestation: 'none',
      algorithms: [-7, -257],
    })
    expect(deps.finishEnroll).toHaveBeenCalledWith({
      challenge: 'ENROLL-CHALLENGE',
      id: 'cred-new',
      clientDataJSON: 'CDA',
      attestationObject: 'ATTOBJ',
      label: 'laptop',
    })
  })

  it('starts with the assertion token when one is given, and sends neither otherwise', async () => {
    const deps = enrollDeps()
    await runEnroll(deps, { label: 'phone', assertionToken: 'tok' })
    expect(deps.startEnroll).toHaveBeenCalledWith({ assertion_token: 'tok' })
    const bare = enrollDeps()
    await runEnroll(bare, { label: 'phone' })
    expect(bare.startEnroll).toHaveBeenCalledWith({})
  })

  it('returns PASSKEYS_MESSAGE without any fetch when not a secure context', async () => {
    const deps = enrollDeps({ secureContext: vi.fn(() => false) })
    const got = await runEnroll(deps, { label: 'laptop', code: '1' })
    expect(got).toEqual({ ok: false, message: PASSKEYS_MESSAGE })
    expect(deps.startEnroll).not.toHaveBeenCalled()
    expect(deps.createCredential).not.toHaveBeenCalled()
    expect(deps.finishEnroll).not.toHaveBeenCalled()
  })
})
