// The one way the run page posts a person's answer or gate decision: to the
// run the request belongs to (a sub-playbook's request reaches the child,
// never the page's run) and naming the request the person saw, so a
// replaced or already settled request is refused instead of answered.
// The next actions panel, the question panel and the review and publish
// cards all go through these.
import { postAnswer, postReview, resolveIntent } from './api'
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

export function postDecision(t: PostTarget, decision: string, workspace = '', note = '') {
  return postReview(t.runId, t.node, decision, note, workspace, t.nonce, t.request)
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
