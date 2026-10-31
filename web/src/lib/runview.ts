// Pure helpers behind the run page's terminal layout: the rail of steps, the
// per-node facts the graph cards show (profile, model, attempt, duration,
// progress), the one-line run log, the decision-layer bars and the counters.
// Everything is read from the run detail the page already has; nothing here
// fetches or keeps state, so each piece is unit-tested on plain data.
import type { AttemptModel, RunDetail } from './api.gen'
import { profileColor, accentVar } from './profilecolor'
import { decisionRows } from './rundecisions'
import { runEventJournal } from './journal'
import type { PlaybookNode, WfEvent } from './types'
import { formatDuration } from './format'
import { profileModelKey, usesDefaultProfile, type ProfileScope } from './profileref'

/** `just now`, `5m ago`, `2h 21m ago`, `3d ago`. */
export function formatAge(ts: number, now: number): string {
  const s = Math.floor((now - ts) / 1000)
  if (s < 30) return 'just now'
  const m = Math.floor(s / 60)
  if (m < 1) return `${s}s ago`
  if (m < 60) return `${m}m ago`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h ${m % 60}m ago`
  return `${Math.floor(h / 24)}d ago`
}

/** `12:09:25`, local time. */
export function clockTime(ts: number): string {
  const d = new Date(ts)
  return [d.getHours(), d.getMinutes(), d.getSeconds()].map((n) => String(n).padStart(2, '0')).join(':')
}

/**
 * Seconds of an `expected_duration` (`90`, `30s`, `5m`, `1h30m`, `1d2h`), the
 * same forms the engine parses; undefined for anything else.
 */
export function parseExpectedDuration(v: unknown): number | undefined {
  if (typeof v === 'number') return Number.isInteger(v) && v >= 0 ? v : undefined
  if (typeof v !== 'string') return undefined
  const s = v.trim()
  if (/^\d+$/.test(s)) return Number(s)
  const m = /^(?:(\d+)d)?(?:(\d+)h)?(?:(\d+)m)?(?:(\d+)s)?$/.exec(s)
  if (!m || !s) return undefined
  const [d, h, mi, se] = m.slice(1).map((x) => (x ? Number(x) : 0))
  return ((d * 24 + h) * 60 + mi) * 60 + se
}

/** Default expected seconds of an agent or script step (the engine's own). */
const DEFAULT_TASK_SECONDS = 120

/** The profile a node binds: its own `profile`, else `defaults.profile` for an agent task. */
export function nodeProfile(node: PlaybookNode, defaults?: Record<string, unknown> | null): string | null {
  const ref = node.profile ?? (usesDefaultProfile(node) ? defaults?.profile : undefined)
  if (typeof ref === 'string') return ref || null
  if (ref && typeof ref === 'object' && typeof (ref as { name?: unknown }).name === 'string')
    return (ref as { name: string }).name
  return null
}

/** The explicit scope (`project` or `global`) of the profile ref `nodeProfile` reads, if any. */
function nodeProfileScope(node: PlaybookNode, defaults?: Record<string, unknown> | null): ProfileScope | undefined {
  const ref = node.profile ?? (usesDefaultProfile(node) ? defaults?.profile : undefined)
  const scope = ref && typeof ref === 'object' ? (ref as { scope?: unknown }).scope : undefined
  return scope === 'project' || scope === 'global' ? scope : undefined
}

/** What the journal says about one node's executions. */
export interface NodeFacts {
  /** Attempt number of the latest `attempt_started`, in journal order. */
  attempt: number
  /** Attempts started over the whole run (re-runs included). */
  attempts: number
  /** When the latest execution started. */
  startedTs?: number
  /**
   * When the latest retry (an attempt above the first) of that execution
   * started: a retry has no `node_started` of its own.
   */
  retryTs?: number
  /** When the latest execution finished; absent while it runs. */
  finishedTs?: number
  /** The latest attempt summary or supervisor note about the node. */
  note?: string
  /** Agent of the latest attempt_started that named one, and that attempt's model. */
  agent?: string
  model?: string
  transcript?: string
}

function str(v: unknown): string | undefined {
  return typeof v === 'string' && v ? v : undefined
}

/** Facts per node id, folded from the event journal in order. */
export function nodeFacts(events: WfEvent[]): Map<string, NodeFacts> {
  const out = new Map<string, NodeFacts>()
  const get = (id: string) => {
    let f = out.get(id)
    if (!f) {
      f = { attempt: 0, attempts: 0 }
      out.set(id, f)
    }
    return f
  }
  for (const e of events) {
    const node = e.node
    if (!node) continue
    const r = e as Record<string, unknown>
    switch (e.type) {
      case 'node_started': {
        const f = get(node)
        f.startedTs = e.ts
        f.finishedTs = undefined
        f.retryTs = undefined
        break
      }
      case 'node_finished':
        get(node).finishedTs = e.ts
        break
      case 'attempt_started': {
        const f = get(node)
        f.attempts += 1
        if (typeof r.attempt === 'number') {
          // Attempts restart at 1 on each visit of a looping node, and
          // arrive in order: any attempt above the first is a retry.
          if (r.attempt > 1) f.retryTs = e.ts
          // The latest attempt, not the highest number: a second visit
          // restarts at 1.
          f.attempt = r.attempt
        }
        f.agent = str(r.agent) ?? f.agent
        // The latest attempt's own model (a hint for a host attempt), never
        // an earlier visit's.
        f.model = str(r.model)
        f.transcript = str(r.transcript) ?? f.transcript
        break
      }
      case 'attempt_finished':
        get(node).note = str(r.summary) ?? get(node).note
        break
      case 'supervisor_action':
        get(node).note = str(r.detail) ?? get(node).note
        break
    }
  }
  return out
}

/** The optional fields the graph node card reads from `data`. */
export interface NodeRunData {
  profile?: string
  /** The model the latest attempt ran on; `null` when it reported none. */
  model?: string | null
  /**
   * With `model: null`: the model the attempt was asked for (the attempt's
   * own hint, else the profile's), shown as a hint only.
   */
  modelHint?: string
  progress?: number
  attempt?: number
  duration?: string
  note?: string
}

/**
 * The latest attempt model of each node: the last row wins, since the rows
 * come in the order the attempts started (a second visit of a looping node
 * restarts its attempt numbers at 1).
 */
function modelsByNode(models: AttemptModel[] | null | undefined): Map<string, AttemptModel> {
  const out = new Map<string, AttemptModel>()
  for (const a of models ?? []) out.set(a.node, a)
  return out
}

/**
 * The run facts of every node for its graph card: profile, the model it ran
 * on (else the profile's), attempt, duration (elapsed while running), the
 * latest note, and for a running node the elapsed share of its expected
 * duration, capped at 95 so a slow step never reads as done.
 */
export function nodeRunData(detail: RunDetail, now: number): Map<string, NodeRunData> {
  const out = new Map<string, NodeRunData>()
  const model = detail.model
  if (!model) return out
  const defaults = (model as { defaults?: Record<string, unknown> | null }).defaults ?? null
  const facts = nodeFacts(detail.events)
  const models = modelsByNode(detail.attempt_models)
  for (const n of model.nodes) {
    const d: NodeRunData = {}
    const profile = nodeProfile(n, defaults)
    if (profile) d.profile = profile
    const f = facts.get(n.id)
    const m = models.get(n.id)
    if (m && !m.model) {
      // The attempt reported no model (a host that named none): keep that
      // state, and what it was asked for only as a hint.
      d.model = null
      const hint = f?.model ?? m.expected ?? undefined
      if (hint) d.modelHint = hint
    } else {
      const mdl = m?.model ?? f?.model ?? undefined
      if (mdl) d.model = mdl
    }
    if (f && f.attempt > 0) d.attempt = f.attempt
    const status = detail.nodes[n.id] ?? ''
    if (f?.startedTs !== undefined) {
      const end = f.finishedTs ?? (status === 'running' ? now : undefined)
      if (end !== undefined) d.duration = formatDuration(end - f.startedTs)
      if (status === 'running') {
        const expected =
          parseExpectedDuration(n.expected_duration) ??
          (n.type === 'agent_task' || n.type === 'script' ? DEFAULT_TASK_SECONDS : undefined)
        if (expected) {
          // The estimate is per attempt: a retry starts its share over.
          const pct = ((now - (f.retryTs ?? f.startedTs)) / (expected * 1000)) * 100
          d.progress = Math.max(0, Math.min(95, Math.round(pct)))
        }
      }
    }
    if (f?.note) d.note = f.note
    out.set(n.id, d)
  }
  return out
}

/** A step's marker in the rail. */
export type StepState = 'done' | 'current' | 'failed' | 'skipped' | 'pending'

export function stepState(status: string | undefined): StepState {
  const s = (status ?? '').toLowerCase()
  if (s === 'succeeded') return 'done'
  if (s === 'running' || s === 'ready' || s === 'interrupted' || s === 'lost' || s === 'unknown') return 'current'
  if (s === 'failed' || s === 'timed_out' || s === 'cancelled') return 'failed'
  if (s === 'skipped') return 'skipped'
  return 'pending'
}

export interface Step {
  id: string
  title: string
  kind: string
  status: string
  state: StepState
}

/** The playbook's nodes in declaration order, each with its rail marker. */
export function runSteps(detail: RunDetail): Step[] {
  return (detail.model?.nodes ?? []).map((n) => {
    const status = detail.nodes[n.id] ?? 'pending'
    return {
      id: n.id,
      title: typeof n.title === 'string' && n.title ? n.title : n.id,
      kind: n.type,
      status,
      state: stepState(status),
    }
  })
}

/** When the run started: its `run_started`, else the first event. */
export function runStartedTs(events: WfEvent[]): number | undefined {
  return (events.find((e) => e.type === 'run_started') ?? events[0])?.ts
}

/** The run's last note: the newest attempt summary or supervisor note. */
export function lastNote(events: WfEvent[]): { node: string | null; text: string } | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i]
    const r = e as Record<string, unknown>
    if (e.type === 'attempt_finished' && str(r.summary)) return { node: e.node ?? null, text: String(r.summary) }
    if (e.type === 'supervisor_action' && str(r.detail)) return { node: e.node ?? null, text: String(r.detail) }
  }
  return null
}

/** One line of the run log. */
export interface LogLine {
  seq: number
  time: string
  actor: string
  /** CSS color of the actor. */
  color: string
  message: string
}

/**
 * The run log: every journal event as one line (time, actor, message). The
 * actor is the node's profile for node events, `decide` for a decision,
 * `supervisor` for wakes and interventions, `host` for host tasks and
 * `engine` for the rest.
 */
export function runLogLines(detail: RunDetail): LogLine[] {
  const defaults = (detail.model as { defaults?: Record<string, unknown> | null } | null)?.defaults ?? null
  const profiles = new Map<string, string | null>(
    (detail.model?.nodes ?? []).map((n) => [n.id, nodeProfile(n, defaults)]),
  )
  const byseq = new Map(detail.events.map((e) => [e.seq, e]))
  return runEventJournal(detail.events).map((j) => {
    const e = byseq.get(j.seq)
    const type = j.type
    let actor = 'engine'
    let color = accentVar('muted')
    if (type === 'decision_made') {
      actor = 'decide'
      color = accentVar('green')
    } else if (type === 'wake_raised' || type.startsWith('supervisor')) {
      actor = 'supervisor'
      color = accentVar('violet')
    } else if (type.startsWith('host_task')) {
      actor = 'host'
      color = accentVar('teal')
    } else if (type === 'review_decided' || type.startsWith('review')) {
      actor = 'review'
      color = accentVar('amber')
    } else if (j.node && profiles.get(j.node)) {
      actor = profiles.get(j.node) as string
      color = profileColor(actor)
    }
    const r = (e ?? {}) as Record<string, unknown>
    let message = type.replace(/_/g, ' ')
    if (type === 'edge_traversed') message = `edge ${String(r.from ?? '')} -> ${String(r.to ?? '')}`
    else if (j.node) message += ` · ${j.node}`
    if (type === 'node_finished' && str(r.status)) message += ` · ${String(r.status)}`
    if (type === 'attempt_finished' && str(r.status)) message += ` · ${String(r.status)}`
    if ((type === 'attempt_started' || type === 'node_started') && typeof r.attempt === 'number')
      message += ` · attempt ${r.attempt}`
    if (type === 'attempt_finished' && str(r.summary)) message += ` · ${String(r.summary)}`
    if (type === 'wake_raised' && str(r.trigger)) message += ` · ${String(r.trigger)}`
    if (type === 'supervisor_action' && str(r.action)) message += ` · ${String(r.action)}`
    if (j.note) message += ` · ${j.note}`
    return { seq: j.seq, time: e ? clockTime(e.ts) : '', actor, color, message }
  })
}

/** One decision as a bar in the decision layer. */
export interface DecisionBar {
  seq: number
  use: string
  node: string | null
  /** The headline answer: `complete`, `p=0.54`, or `error`. */
  value: string
  /** How decisive the answer is, 0..1: max(p, 1-p), or the confidence. */
  strength: number
  /**
   * `sharp` (decisive or applied), `split` (undecided) or `error` (the
   * provider failed). Whether it was applied is `outcome`, not the verdict.
   */
  verdict: 'sharp' | 'split' | 'error'
  /** Mode and whether it was applied: `applied`, `shadow`, ... */
  outcome: string
  /** Every answer of the decision, or the error. */
  answer: string
  source: string
  latency: string
}

/** A decision counts as sharp at this decisiveness. */
export const SHARP_AT = 0.75

/** The decision-layer bars, one per `decision_made`, journal order. */
export function decisionBars(events: WfEvent[]): DecisionBar[] {
  const rows = new Map(decisionRows(events).map((r) => [r.seq, r]))
  return events
    .filter((e) => e.type === 'decision_made')
    .map((e) => {
      const r = e as Record<string, unknown>
      const row = rows.get(e.seq)!
      let strength = 0
      let value = ''
      const answers = r.answers && typeof r.answers === 'object' ? Object.values(r.answers as object) : []
      const first = answers.find((a) => a && typeof a === 'object') as Record<string, unknown> | undefined
      if (first) {
        const p = typeof first.p === 'number' ? first.p : undefined
        const conf = typeof first.confidence === 'number' ? first.confidence : undefined
        strength = conf ?? (p !== undefined ? Math.max(p, 1 - p) : 0)
        if (first.value !== undefined && first.value !== null) value = String(first.value)
        else if (p !== undefined) value = `p=${p.toFixed(2)}`
      }
      const error = str(r.error)
      if (error) {
        value = 'error'
        strength = 0
      }
      const sharp = !error && (r.applied === true || strength >= SHARP_AT)
      return {
        seq: e.seq,
        use: row.use,
        node: row.node,
        value,
        strength: Math.max(0, Math.min(1, strength)),
        verdict: error ? 'error' : sharp ? 'sharp' : 'split',
        outcome: row.outcome,
        answer: row.answer,
        source: row.source,
        latency: row.latency,
      }
    })
}

/** The provider/model and mode line of the decision layer: `shadow · typesafe/jev-1.13.0`. */
export function decisionSource(events: WfEvent[]): { mode: string; source: string } | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i]
    if (e.type !== 'decision_made') continue
    const r = e as Record<string, unknown>
    const provider = str(r.provider) ?? 'no provider'
    const model = str(r.model)
    return { mode: str(r.mode) ?? '', source: model ? `${provider}/${model}` : provider }
  }
  return null
}

/** The profile of each node that has one, and the model it ran on when known. */
export function runProfiles(detail: RunDetail): { name: string; scope?: ProfileScope; key: string; model?: string }[] {
  const defaults = (detail.model as { defaults?: Record<string, unknown> | null } | null)?.defaults ?? null
  const models = modelsByNode(detail.attempt_models)
  const out = new Map<string, { name: string; scope?: ProfileScope; key: string; model?: string }>()
  for (const n of detail.model?.nodes ?? []) {
    const name = nodeProfile(n, defaults)
    if (!name) continue
    const scope = nodeProfileScope(n, defaults)
    const key = profileModelKey(name, scope)
    const m = models.get(n.id)
    const mdl = m?.model ?? m?.expected ?? undefined
    const prev = out.get(key)
    if (!prev) out.set(key, { name, ...(scope ? { scope } : {}), key, ...(mdl ? { model: mdl } : {}) })
    else if (!prev.model && mdl) prev.model = mdl
  }
  return [...out.values()]
}

/**
 * `max_parallel` from the playbook's defaults, which wins over everything
 * else. Without one the run config or the engine default (4) applies; the
 * run detail carries neither, so that case says it is the default.
 */
export function maxParallel(detail: RunDetail): { value: number; isDefault: boolean } {
  const v = (detail.model as { defaults?: { max_parallel?: unknown } | null } | null)?.defaults?.max_parallel
  return typeof v === 'number' && v > 0 ? { value: v, isDefault: false } : { value: 4, isDefault: true }
}

/** The node the run is on: waiting_on, else the first running node, else the last that started. */
export function currentStage(detail: RunDetail): string | null {
  // `waiting_on` may name a node of a child run; this run's own node wins.
  const waitingOn = detail.progress?.waiting_on
  if (waitingOn && waitingOn in detail.nodes) return waitingOn
  const running = Object.entries(detail.nodes).find(([, s]) => s === 'running')
  if (running) return running[0]
  if (waitingOn) return waitingOn
  for (let i = detail.events.length - 1; i >= 0; i--) {
    const e = detail.events[i]
    if (e.type === 'node_started' && e.node) return e.node
  }
  return null
}

/** How a run edge looks: walked, leading into a running node, or not reached. */
export type EdgeRunState = 'walked' | 'active' | 'idle'

/**
 * The run state of each edge `from -> to`: `active` when the journal
 * traversed it into a node that is running now (since that node last
 * finished, so a loop lights only the edge that led back), `walked` once the
 * journal traversed it, `idle` otherwise. A journal with no traversals at all
 * (an older one) falls back to "the source is done".
 */
export function edgeRunState(
  events: WfEvent[],
  nodes: Record<string, string>,
): (from: string, to: string) => EdgeRunState {
  const walked = new Set<string>()
  // Edges traversed into each node since it last finished.
  const into = new Map<string, Set<string>>()
  for (const e of events) {
    if (e.type === 'node_finished' && e.node) {
      into.delete(e.node)
      continue
    }
    if (e.type !== 'edge_traversed') continue
    const r = e as Record<string, unknown>
    const key = `${String(r.from)}\u0000${String(r.to)}`
    walked.add(key)
    const set = into.get(String(r.to)) ?? new Set<string>()
    set.add(key)
    into.set(String(r.to), set)
  }
  return (from, to) => {
    const key = `${from}\u0000${to}`
    const ran = walked.size ? into.get(to)?.has(key) === true : stepState(nodes[from]) === 'done'
    if (nodes[to] === 'running' && ran) return 'active'
    return walked.has(key) ? 'walked' : 'idle'
  }
}
