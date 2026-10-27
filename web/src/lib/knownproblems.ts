// Known problems on the dashboard: how a playbook's problems group and read
// on its page (KnownProblemsCard) and how a run's shown and hidden problems
// read on the run page. Pure functions over the generated types.

import type {
  ProblemOrigin,
  ProblemScope,
  ScopeKind,
  ProblemView,
  RecheckBasis,
  RecheckRecord,
  RecheckVerdict,
  ReportedText,
  RunKnownProblems,
  SourceNow,
} from './api.gen'
import { QUOTE_MAX, QUOTE_MIN } from './consts.gen'
import type { Tone } from './runverdict'

/** A playbook's problems, in the order the card lists them. */
export interface ProblemGroups {
  /** Open and shown to a run that starts now. */
  shown: ProblemView[]
  /** Open, and a person should look: a recheck could not settle it, it
   * proposes a close, or its source changed or is gone. */
  look: ProblemView[]
  /** Open, but not shown yet: seen by fewer runs than the block asks. */
  waiting: ProblemView[]
  /** Closed or dismissed. */
  done: ProblemView[]
}

/** Whether a person should look at `p` (the server says so: an unsettled
 * recheck, a proposed close, or a changed or unreadable source). */
export function needsLook(p: ProblemView): boolean {
  return p.status === 'open' && p.needs_look
}

/** `problems` grouped for the card, each group in store order (most runs
 * first, then the latest seen). */
export function groupProblems(problems: ProblemView[]): ProblemGroups {
  const groups: ProblemGroups = { shown: [], look: [], waiting: [], done: [] }
  for (const p of problems) {
    if (p.status !== 'open') groups.done.push(p)
    else if (needsLook(p)) groups.look.push(p)
    else if (p.significant) groups.shown.push(p)
    else groups.waiting.push(p)
  }
  return groups
}

/** The tag colour of a problem: shown ones fail, the ones a person should
 * look at are gaps, closed ones pass, the rest are neutral. */
export function problemTone(p: ProblemView): Tone {
  if (p.status === 'closed') return 'pass'
  if (p.status === 'dismissed') return 'neutral'
  if (needsLook(p)) return 'gap'
  return p.significant ? 'fail' : 'neutral'
}

/** A problem's state in a word or two. */
export function problemStateText(p: ProblemView): string {
  if (p.status !== 'open') return p.status
  if (needsLook(p)) return 'needs a look'
  return p.significant ? 'shown' : 'not shown yet'
}

const PREFIX: Record<ScopeKind, string> = {
  connector: 'connector',
  mcp_tool: 'mcp',
  cli: 'cli',
  skill: 'skill',
  path: 'path',
  node: 'node',
  other: 'other',
}

/** A problem's scope as a finding's location writes it:
 * `connector:crm.update_contact`, `path:scripts/build.sh`. */
export function scopeText(s: ProblemScope): string {
  return `${PREFIX[s.kind]}:${s.name}`
}

/** `1 run`, `3 runs`. */
export function runsText(n: number): string {
  return `${n} ${n === 1 ? 'run' : 'runs'}`
}

/** Where a problem came from: a finding of its kind, or connector calls
 * that failed alike. */
export function originText(o: ProblemOrigin): string {
  if (o.type === 'finding') return `finding: ${o.kind.replace(/_/g, ' ')}`
  const http = o.http_status ? `, HTTP ${o.http_status}` : ''
  return `calls failed: ${o.outcome.replace(/_/g, ' ')}${http}`
}

/** What the source's state means for a person; null when nothing changed. */
export function sourceNowText(s: SourceNow | undefined): string | null {
  switch (s) {
    case 'changed':
      return 'Its source changed since it was last checked: a run is not shown it until a recheck or a person settles it.'
    case 'unreadable':
      return 'Its source cannot be read now (gone or moved); that never closes a problem.'
    default:
      return null
  }
}

const VERDICTS: Record<RecheckVerdict, string> = {
  holds: 'still holds',
  closed: 'closed',
  closed_proposed: 'close proposed',
  undetermined: 'undetermined',
}

const BASES: Record<RecheckBasis, string> = {
  unchanged: 'the unchanged source',
  model: 'the decision model',
  host: 'a host',
  person: 'a person',
}

/** A recheck in one line: `close proposed by a host (quote new)`. */
export function checkText(c: RecheckRecord): string {
  return `${VERDICTS[c.verdict]} by ${BASES[c.basis]} (${c.reason_code.replace(/_/g, ' ')})`
}

/** Why a quote cannot close a problem before it is sent, or null. The
 * server checks the rest: found in the new text, absent from the old. */
export function quoteProblem(quote: string): string | null {
  const n = [...quote].length
  if (n < QUOTE_MIN) return `A quote needs at least ${QUOTE_MIN} characters.`
  if (n > QUOTE_MAX) return `A quote has at most ${QUOTE_MAX} characters.`
  return null
}

/** What a recheck came to, as a toast says it: the verdict in words and
 * whether it settled anything (`undetermined` did not; a proposed close
 * waits for a person). */
export function recheckResult(c: RecheckRecord): { text: string; tone: 'success' | 'info' | 'warning' } {
  const text = `Problem rechecked: ${checkText(c)}`
  if (c.verdict === 'undetermined') return { text, tone: 'warning' }
  if (c.verdict === 'closed_proposed') return { text, tone: 'info' }
  return { text, tone: 'success' }
}

/** What the close form says when there is no new passage to pick: a
 * problem without a readable source (none, or gone now) closes on the
 * person's word. */
export function closeHint(p: ProblemView): string {
  if (!p.source || p.source_now === 'unreadable') {
    return `This problem has no readable source: the close is taken on your word, with a quote of ${QUOTE_MIN} to ${QUOTE_MAX} characters.`
  }
  return 'Its source has no passage new since the problem was seen.'
}

/** The wordings runs reported besides the leading one, most runs first. */
export function otherWordings(texts: ReportedText[] | undefined, leading: string | undefined): ReportedText[] {
  return (texts ?? []).filter((t) => t.text !== leading).sort((a, b) => b.runs - a.runs)
}

/** The line for problems of a file this installation did not write. */
export function unsignedText(n: number): string {
  return `${n} ${n === 1 ? 'problem' : 'problems'} in a file this apb installation did not write: never shown to runs and not listed.`
}

/** Whether a person may act on `p` in each way. A problem whose source
 * cannot be read now closes only on the person's word, and the server
 * cannot record that it still holds. */
export function actionsFor(p: ProblemView): {
  confirm: boolean
  dismiss: boolean
  reopen: boolean
  close: boolean
  holds: boolean
  recheck: boolean
} {
  const open = p.status === 'open'
  const readable = p.source_now !== 'unreadable'
  return {
    confirm: open && !p.confirmed,
    dismiss: open,
    reopen: !open,
    close: open,
    holds: open && readable && (p.source_now === 'changed' || p.needs_look),
    recheck: open && p.source_now === 'changed',
  }
}

const HIDDEN: Record<string, string> = {
  source_changed_unchecked: 'its source changed and was not rechecked',
  undetermined: 'a recheck could not settle it',
  closed_proposed: 'a close is proposed and waits for a person',
  over_limit: 'more problems than the playbook shows at once',
  source_unreadable: 'its source cannot be read now',
  no_matching_node: 'no agent node of this run binds its connector, carries its skill or is its node',
}

/** Why a run did not show a problem, in words. */
export function hiddenReasonText(reason: string): string {
  return HIDDEN[reason] ?? reason.replace(/_/g, ' ')
}

/** Who let a shown problem stand, in words. */
export function shownBasisText(basis: string): string {
  return basis === 'unchanged' ? 'source unchanged' : `rechecked by ${BASES[basis as RecheckBasis] ?? basis}`
}

/** The nodes a run showed a problem to, in words. */
export function shownNodesText(nodes: string[] | undefined): string {
  return nodes?.length ? `to ${nodes.join(', ')}` : 'to every agent node'
}

/** Whether a run's block has anything to show on the run page. */
export function hasRunProblems(k: RunKnownProblems | undefined): k is RunKnownProblems {
  return !!k && (k.shown.length > 0 || k.hidden.length > 0 || k.unsigned > 0)
}
