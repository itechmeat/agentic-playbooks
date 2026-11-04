// The passkey logic the dashboard shares: the secure-context check, the
// assertion purpose payloads the server's routes read, and the two
// ceremonies (a review that may need a passkey, and an enrollment) with
// every effect injected, so tests run without a real navigator or network.
//
// All strings the browser WebAuthn API hands back (clientDataJSON,
// attestationObject, authenticatorData, signature) are posted as base64url
// of the exact bytes the API produced, which is what the routes decode.

/** The exact message the UI shows when the page is not a secure context. */
export const PASSKEYS_MESSAGE = 'Passkeys need https or localhost'

/** True when the page may call the WebAuthn API: a secure context, or localhost. */
/** Decodes the base64url challenge the server issued into the raw bytes the
 * WebAuthn API wants. The challenge string itself is not the challenge. */
export function challengeBytes(challenge: string): Uint8Array<ArrayBuffer> {
  const normalized = challenge.replace(/-/g, '+').replace(/_/g, '/')
  const padded = normalized + '='.repeat((4 - (normalized.length % 4)) % 4)
  const binary = atob(padded)
  const bytes = new Uint8Array(new ArrayBuffer(binary.length))
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
  return bytes
}

export function secureContextOk(): boolean {
  const w = typeof window !== 'undefined' ? window : undefined
  if (w && typeof w.isSecureContext === 'boolean') return w.isSecureContext
  if (typeof location === 'undefined') return false
  const host = location.hostname
  return (
    host === 'localhost' ||
    host === '127.0.0.1' ||
    host === '::1' ||
    host === '[::1]' ||
    host.endsWith('.localhost')
  )
}

/** The assertion purpose payloads: exactly one key, as the server routes read. */
export type AssertPurpose =
  | { decision: { run: string; node: string; decision: string; request: number | null; nonce: string | null } }
  | { enroll_new: Record<string, never> }
  | { remove: string }

export function decisionPurpose(
  run: string,
  node: string,
  decision: string,
  request?: number | null,
  nonce?: string | null,
): AssertPurpose {
  return {
    decision: {
      run,
      node,
      decision,
      request: request ?? null,
      nonce: nonce ?? null,
    },
  }
}

export function enrollNewPurpose(): AssertPurpose {
  return { enroll_new: {} }
}

export function removePurpose(label: string): AssertPurpose {
  return { remove: label }
}

/** What an injected review post answers: success, or the status and message. */
export type Refusal = { ok: false; status: number; message: string }
export type PostOutcome = { ok: true } | Refusal

/** The body a review post sends; `passkey_token` rides on the retried post. */
export interface ReviewBody {
  node: string
  decision: string
  note?: string
  nonce?: string
  request?: number
  passkey_token?: string
  /** The assertion the token was minted from, so the drive re-verifies it. */
  assertionId?: string
  assertionClientDataJSON?: string
  assertionAuthenticatorData?: string
  assertionSignature?: string
}

export interface AssertionResult {
  id: string
  clientDataJSON: string
  authenticatorData: string
  signature: string
}

export interface DecisionDeps {
  postReview: (runId: string, body: ReviewBody) => Promise<PostOutcome>
  assertChallenge: (purpose: AssertPurpose) => Promise<{ challenge: string }>
  verifyAssertion: (
    purpose: AssertPurpose,
    challenge: string,
    assertion: AssertionResult,
  ) => Promise<{ token: string }>
  getAssertion: (challenge: string) => Promise<AssertionResult>
  /** Injectable so the not-secure-context path is testable. */
  secureContext?: () => boolean
}

/**
 * Posts the review; on a 403 whose message names a passkey requirement, runs
 * the assertion ceremony and posts again with the token. Any other refusal
 * (or a failing ceremony) is returned as is.
 */
export async function runDecisionWithPasskey(
  deps: DecisionDeps,
  runId: string,
  body: ReviewBody,
): Promise<PostOutcome> {
  const first = await deps.postReview(runId, body)
  if (first.ok) return first
  if (first.status !== 403 || !first.message.includes('needs a passkey')) return first
  const secure = deps.secureContext ?? secureContextOk
  if (!secure()) return { ok: false, status: 403, message: PASSKEYS_MESSAGE }
  const purpose = decisionPurpose(runId, body.node, body.decision, body.request, body.nonce)
  const { challenge } = await deps.assertChallenge(purpose)
  const assertion = await deps.getAssertion(challenge)
  const { token } = await deps.verifyAssertion(purpose, challenge, assertion)
  // The retry carries the token the verify step handed back and the
  // assertion it was minted from, so the drive re-verifies the stamp when
  // it consumes the decision.
  return deps.postReview(runId, {
    ...body,
    passkey_token: token,
    assertionId: assertion.id,
    assertionClientDataJSON: assertion.clientDataJSON,
    assertionAuthenticatorData: assertion.authenticatorData,
    assertionSignature: assertion.signature,
  })
}

export interface EnrollStart {
  challenge: string
  rpId: string
  userVerification: string
  attestation: string
  algorithms: number[]
}

export interface EnrollEntry {
  label: string
  created_at: string
  id_prefix: string
}

export interface CreatedCredential {
  id: string
  clientDataJSON: string
  attestationObject: string
}

export interface EnrollDeps {
  startEnroll: (body: { code?: string; assertion_token?: string }) => Promise<EnrollStart>
  finishEnroll: (body: {
    challenge: string
    id: string
    clientDataJSON: string
    attestationObject: string
    label: string
  }) => Promise<EnrollEntry>
  createCredential: (options: {
    challenge: string
    rpId: string
    userVerification: string
    attestation: string
    algorithms: number[]
  }) => Promise<CreatedCredential>
  /** Injectable so the not-secure-context path is testable. */
  secureContext?: () => boolean
}

export type EnrollOutcome = { ok: true; entry: EnrollEntry } | { ok: false; message: string }

/**
 * The enrollment ceremony: start (with a one-time code or an assertion
 * token), create the credential, finish. Returns PASSKEYS_MESSAGE without
 * any fetch when the page is not a secure context.
 */
export async function runEnroll(
  deps: EnrollDeps,
  opts: { label: string; code?: string; assertionToken?: string },
): Promise<EnrollOutcome> {
  const secure = deps.secureContext ?? secureContextOk
  if (!secure()) return { ok: false, message: PASSKEYS_MESSAGE }
  const start = await deps.startEnroll({
    ...(opts.code ? { code: opts.code } : {}),
    ...(opts.assertionToken ? { assertion_token: opts.assertionToken } : {}),
  })
  const credential = await deps.createCredential({
    challenge: start.challenge,
    rpId: start.rpId,
    userVerification: start.userVerification,
    attestation: start.attestation,
    algorithms: start.algorithms,
  })
  const entry = await deps.finishEnroll({
    challenge: start.challenge,
    id: credential.id,
    clientDataJSON: credential.clientDataJSON,
    attestationObject: credential.attestationObject,
    label: opts.label,
  })
  return { ok: true, entry }
}
