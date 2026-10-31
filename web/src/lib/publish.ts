// The publish step as the dashboard shows it: the exact plan a person
// consents to, the push's ledger state after it, and the node form's
// choices. Pure helpers, so the panel and the form only render.

import type { PlanView, PublishCommit, PublishView, PushView } from './api.gen'
import { PUBLISH_REFUSAL_TEXTS } from './consts.gen'

/** A publish gate waiting for a person: the node and the plan to consent to. */
export interface PublishGate {
  node: string
  plan: PlanView
  /** The node's guidance for the person, shown literally. */
  prompt?: string
}

/** A publish node as the panel shows it: its run block, and the guidance of an open gate. */
export type PublishCard = PublishView & { prompt?: string }

/** A node of the playbook as the publish source picker and skeleton read it. */
export interface SourceNode {
  id: string
  type: string
  list?: unknown
}

/** The first 12 characters of a commit id. */
export function shortSha(sha: string): string {
  return sha.slice(0, 12)
}

/** The tags of one commit: whether this run made it, and its Apb-Run trailers. */
export function commitTags(c: PublishCommit): string[] {
  return [c.this_run ? 'this run' : 'before the run', ...(c.apb_run ?? []).map((r) => `Apb-Run: ${r}`)]
}

/** What the remote holds now, in words. */
export function remoteState(plan: PlanView): string {
  return plan.remote_before
    ? `Branch ${plan.branch} is at ${shortSha(plan.remote_before)}; the push moves it forward.`
    : `Branch ${plan.branch} does not exist on the remote yet; the push creates it.`
}

/** How many commits the push carries, and how many of them, listed or
 * omitted, this run made. */
export function commitCount(plan: PlanView): string {
  const all = plan.commits.length + plan.omitted
  return `${all} ${all === 1 ? 'commit' : 'commits'}, ${plan.this_run} made by this run`
}

/** The sentence of a pin refusal code, or the code itself when unknown. */
export function refusalText(code: string): string {
  return (PUBLISH_REFUSAL_TEXTS as Record<string, string>)[code] ?? code
}

/** The push's ledger state in words, with what that push sent (its own
 * plan, which may be an earlier visit's than the plan shown above it). A
 * state this dashboard does not know is shown as written. */
export function pushLine(push: PushView): string {
  const words: Record<string, string> = {
    pending: 'in flight',
    confirmed: 'confirmed on the remote',
    not_sent: 'not sent',
    unknown: 'result unknown',
  }
  const what = push.after ? ` of ${shortSha(push.after)} to branch ${push.branch}` : ''
  const detail = push.detail ? `: ${push.detail}` : ''
  return `Push ${push.intent}${what}: ${words[push.state] ?? push.state}${detail}`
}

/** `value` as one POSIX shell word: left as is when it holds only safe
 * characters, else in single quotes (a single quote closed, escaped and
 * reopened), so a copied command never splits or runs a value. */
export function shellWord(value: string): string {
  if (/^[A-Za-z0-9._\/:=@%+-]+$/.test(value)) return value
  return `'${value.replace(/'/g, `'\\''`)}'`
}

/** The terminal command that consents to exactly this plan, every value
 * quoted for the shell. */
export function approveCommand(runId: string, node: string, nonce: string): string {
  return ['apb', 'publish', 'approve', runId, '--node', node, '--nonce', nonce].map(shellWord).join(' ')
}

/** The command a person runs after checking the remote for an unknown push. */
export function resolveCommand(runId: string, push: PushView): string {
  return `apb intent resolve ${shellWord(runId)} ${shellWord(push.intent)} --sent|--not-sent`
}

/** The push's result is unknown: a person has to look. */
export function needsALook(v: PublishView): boolean {
  return v.push?.state === 'unknown'
}

/**
 * Every publish node the panel shows: the run's publish block, with the
 * plan of an open gate taken from the pending review (the plan the review
 * route checks the nonce against). Only a card built from an open gate
 * offers a decision: a run that stopped with a request left open has no
 * gate, so its card shows the plan without the buttons.
 */
export function publishCards(views: PublishView[] | null | undefined, gates: PublishGate[]): PublishCard[] {
  const withGate = (v: PublishView, g: PublishGate): PublishCard => {
    const card: PublishCard = { ...v, plan: g.plan, awaiting_consent: true }
    if (g.prompt) card.prompt = g.prompt
    return card
  }
  const list: PublishCard[] = (views ?? []).map((v) => {
    const gate = gates.find((g) => g.node === v.node)
    return gate ? withGate(v, gate) : { ...v, awaiting_consent: false }
  })
  for (const g of gates) {
    if (!list.some((v) => v.node === g.node)) list.push(withGate({ node: g.node, remote: '', awaiting_consent: true }, g))
  }
  return list
}

/** The node kinds a publish node may name as `from`. */
const SOURCE_KINDS = new Set(['agent_task', 'script'])

/** The nodes a publish node may publish from: agent tasks and scripts, not
 * list nodes. The validator also needs the publish node to come after the
 * source (V116); the form says so, since the picker does not read edges. */
export function publishSources(nodes: SourceNode[], self: string): string[] {
  return nodes.filter((n) => SOURCE_KINDS.has(n.type) && !n.list && n.id !== self).map((n) => n.id)
}

/** The fields a new publish node starts with: the remote, and the last eligible source. */
export function publishSkeleton(nodes: SourceNode[]): Record<string, unknown> {
  const sources = publishSources(nodes, '')
  const from = sources[sources.length - 1]
  return from ? { from, remote: 'origin' } : { remote: 'origin' }
}
