// The one way the run page posts a person's answer or gate decision: to the
// run the request belongs to (a sub-playbook's request reaches the child,
// never the page's run) and naming the request the person saw, so a
// replaced or already settled request is refused instead of answered.
// The next actions panel, the question panel and the review and publish
// cards all go through these.
//
// A decision on a person-only gate or an approval decides through a fresh
// passkey assertion when the server says one is enrolled: the first post
// is refused naming the passkey, the ceremony runs here (lib/passkeys),
// and the decision is posted again with the token it minted.
import { postAnswer, postReview, resolveIntent } from './api'
import { ApiError, jsonHeaders, qs, requestJson } from './api/http'
import {
  PASSKEYS_MESSAGE,
  challengeBytes,
  runDecisionWithPasskey,
  secureContextOk,
  type AssertionResult,
  type AssertPurpose,
  type PostOutcome,
  type ReviewBody,
} from './passkeys'
import type { NextAction } from './api.gen'

export interface PostTarget {
  /** The run the request belongs to. */
  runId: string
  node: string
  /** The journal seq of the request (`target.request.seq`). */
  request?: number
  /** A publish gate's plan nonce. */
  nonce?: string
}

/** The target of a next action: its own run, node, request and nonce. */
export function actionTarget(a: NextAction): PostTarget {
  const t: PostTarget = { runId: a.run_id, node: a.target.node ?? '' }
  if (a.target.request) t.request = a.target.request.seq
  const nonceArg = a.args.find((x) => x.name === 'nonce')?.value
  const nonce = a.target.request?.nonce ?? (nonceArg?.kind === 'given' ? nonceArg.value : undefined)
  if (nonce) t.nonce = nonce
  return t
}

/** Overrides for the passkey ceremony, so tests run without a browser. */
export interface DecisionCeremony {
  secureContext?: () => boolean
  assertChallenge: (purpose: AssertPurpose) => Promise<{ challenge: string }>
  getAssertion: (challenge: string) => Promise<AssertionResult>
  verifyAssertion: (
    purpose: AssertPurpose,
    challenge: string,
    assertion: AssertionResult,
  ) => Promise<{ token: string }>
}

/** Base64url of an ArrayBuffer: the string shape every passkey route
 * reads out of the browser API's binary fields (no +, / or padding). */
export function b64u(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf)
  let s = ''
  for (const b of bytes) s += String.fromCharCode(b)
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

async function realGetAssertion(challenge: string): Promise<AssertionResult> {
  const cred = await navigator.credentials.get({
    publicKey: {
      challenge: challengeBytes(challenge),
      rpId: location.hostname,
      userVerification: 'required',
    },
  })
  if (!cred) throw new Error('no credential returned')
  const r = (cred as PublicKeyCredential).response as AuthenticatorAssertionResponse
  return {
    id: cred.id,
    clientDataJSON: b64u(r.clientDataJSON),
    authenticatorData: b64u(r.authenticatorData),
    signature: b64u(r.signature),
  }
}

/** The browser's ceremony for a decision. A decision names its run's
 * project with `workspace`, exactly like the review post it precedes: the
 * dashboard pins no project root. */
export function realCeremony(workspace: string): DecisionCeremony {
  const query = qs({ workspace })
  const post = <T>(url: string, body: unknown) =>
    requestJson<T>(url, { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) })
  return {
    assertChallenge: (purpose) => post<{ challenge: string }>(`/api/passkeys/assert/challenge${query}`, { purpose }),
    getAssertion: realGetAssertion,
    verifyAssertion: (purpose, challenge, assertion) =>
      post<{ token: string }>(`/api/passkeys/assert/verify${query}`, { purpose, challenge, ...assertion }),
  }
}

export async function postDecision(
  t: PostTarget,
  decision: string,
  workspace = '',
  note = '',
  overrides?: Partial<DecisionCeremony>,
) {
  const body: ReviewBody = { node: t.node, decision, note }
  if (t.nonce) body.nonce = t.nonce
  if (t.request !== undefined) body.request = t.request
  let original: unknown
  let postedSeq = 0
  const outcome: PostOutcome = await runDecisionWithPasskey(
    {
      postReview: async (runId, b) => {
        try {
          const assertion = b.assertionId
            ? {
                assertionId: b.assertionId,
                assertionClientDataJSON: b.assertionClientDataJSON,
                assertionAuthenticatorData: b.assertionAuthenticatorData,
                assertionSignature: b.assertionSignature,
              }
            : undefined
          const got = await postReview(runId, b.node, b.decision, b.note ?? '', workspace, b.nonce, b.request, b.passkey_token, assertion)
          postedSeq = got.posted_seq
          return { ok: true }
        } catch (e) {
          if (e instanceof ApiError) {
            original = e
            return { ok: false, status: e.status, message: e.message }
          }
          throw e
        }
      },
      ...realCeremony(workspace),
      ...overrides,
      secureContext: overrides?.secureContext ?? secureContextOk,
    },
    t.runId,
    body,
  )
  if (outcome.ok) return { posted_seq: postedSeq }
  // The ceremony path reports an insecure context as a plain refusal;
  // every other refusal is the server's own error, rethrown as it was.
  if (outcome.message === PASSKEYS_MESSAGE) throw new Error(PASSKEYS_MESSAGE)
  if (original) throw original
  throw new Error(outcome.message)
}

export function postReply(t: PostTarget, answer: string, workspace = '') {
  const body: { node?: string; answer: string; request?: number } = { answer }
  if (t.node) body.node = t.node
  if (t.request !== undefined) body.request = t.request
  return postAnswer(t.runId, body, workspace)
}

/** A person's decision on an unknown send of `runId`. */
export function postIntentDecision(runId: string, intent: string, sent: boolean, workspace = '') {
  return resolveIntent(runId, intent, sent, '', workspace)
}
