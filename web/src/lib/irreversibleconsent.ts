// 0.24.0 irreversible consent: what the Run dialog does with a refusal of
// `irreversible_requires_confirmation`. It asks the person only when the
// refusal carries a `consent_nonce` the dialog has not just sent: without a
// nonce there is nothing a confirmation could send that the server would
// accept, and a refusal of the nonce just sent would ask the same question
// again, so both show the refusal and stop instead of looping.

export type ConsentStep =
  | { kind: 'ask'; sources: string[]; nonce: string }
  | { kind: 'stop'; message: string }

export function consentStep(
  body: Record<string, unknown> | undefined,
  sentNonce?: string,
): ConsentStep {
  const raw = body?.sources
  const sources = Array.isArray(raw) ? raw.map(String) : []
  const nonce = typeof body?.consent_nonce === 'string' ? body.consent_nonce : ''
  if (nonce && nonce !== sentNonce) return { kind: 'ask', sources, nonce }
  const detail = typeof body?.detail === 'string' ? body.detail : ''
  const reason = typeof body?.reason === 'string' ? body.reason : ''
  const why = [reason, detail].filter(Boolean).join('; ')
  return {
    kind: 'stop',
    message: why || 'The server refused the run and offered no consent to confirm.',
  }
}
