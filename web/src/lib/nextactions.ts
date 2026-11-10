// A run's next actions as the dashboard shows them: which ones the page
// runs itself, the `apb` command line of the others (the server's own
// rendering, the line the CLI prints, carried in `command`), what a wait
// does by itself, and the lines of a refused start.
import { ApiError } from './api/http'
import type {
  ActionCode,
  AutoOutcome,
  AutoProgress,
  NextAction,
  Remedy,
  RunActions,
  StartAction,
  UnattendedOutcome,
} from './api.gen'

/** The codes the page has a button for. */
export const EXECUTABLE: readonly ActionCode[] = [
  'answer_question',
  'decide_review',
  'decide_publish',
  'resolve_intent',
  'resume_run',
  'retry_now',
]

export function isExecutable(a: NextAction): boolean {
  return EXECUTABLE.includes(a.code)
}

/** A person may take it from the page: the run accepts it from a person. */
export function personTakes(a: NextAction): boolean {
  return a.principals.includes('person')
}

/** The actions in their primary order, each with whether it is the primary
 * one. Absent on a payload from an older server. */
export function orderedActions(actions: RunActions | null | undefined): { action: NextAction; primary: boolean }[] {
  if (!actions) return []
  return actions.actions.map((action, i) => ({ action, primary: actions.primary === i }))
}

/** The key of a control the run page shows for one of its own run's
 * waits outside the next-actions panel: the code of the action it takes
 * and the node it is for. */
export function controlKey(code: ActionCode, node?: string | null): string {
  return `${code}:${node ?? ''}`
}

/** The controls the run page shows for its own run outside the panel: the
 * question panel, the human review card, the publish panel (each per node)
 * and the resume button of the pause controls (with its budget form). */
export function pageControls(
  gates: { questions: { node: string }[]; reviews: { node: string }[]; publishes: { node: string }[] },
  resumable: boolean,
): string[] {
  return [
    ...gates.questions.map((q) => controlKey('answer_question', q.node)),
    ...gates.reviews.map((r) => controlKey('decide_review', r.node)),
    ...gates.publishes.map((p) => controlKey('decide_publish', p.node)),
    ...(resumable ? [controlKey('resume_run'), controlKey('resume_with_budget')] : []),
  ]
}

/** Whether another control on the page already takes `a`: an action of the
 * page's own run (`runId`) whose code and node one of `controls` names. A
 * sub-playbook run's action has no other control on the page. */
export function shownElsewhere(a: NextAction, runId: string, controls: readonly string[]): boolean {
  const node = a.code === 'resume_run' || a.code === 'resume_with_budget' ? null : a.target.node
  return a.run_id === runId && controls.includes(controlKey(a.code, node))
}

/** The `apb` command line of an action as the server rendered it, or null
 * for one the CLI has no command for (the supervisor's). */
export function cliLine(a: NextAction): string | null {
  return a.command ?? null
}

/** `1h 2m`, `3m 4s`, `5s`. */
export function durationWords(ms: number): string {
  const s = Math.floor(ms / 1000)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  if (h === 0 && m === 0) return `${sec}s`
  if (h === 0) return `${m}m ${sec}s`
  return `${h}h ${m}m`
}

const OUTCOME: Record<AutoOutcome, string> = {
  answered_default: 'its default answer is posted',
  timer_elapses: 'the timer elapses',
  attempt_fails: 'the attempt fails',
  wait_times_out: 'the wait times out and fails',
  resume_attempt: 'apb tries to resume the run',
  retry_attempt: "the node's retry starts",
  run_deadline: 'the run ends at its deadline',
}

/** What a wait does by itself, with the time in the viewer's local time. */
export function autoLine(auto: AutoProgress, fmt: (ms: number) => string = localTime): string {
  const what = OUTCOME[auto.outcome] ?? auto.outcome
  if (auto.kind === 'at') {
    return `continues by itself at ${fmt(auto.at_ms)}${auto.assured ? '' : ' (not assured)'}: ${what}`
  }
  const clock = auto.outcome === 'retry_attempt' ? 'delay' : 'timer'
  return `${clock} frozen while paused, ${durationWords(auto.remaining_ms)} left: then ${what}`
}

/** What an unattended run does at this wait, the line the CLI prints too. */
export function unattendedLine(u: UnattendedOutcome): string {
  switch (u.kind) {
    case 'proceeds':
      return `unattended: goes on by itself (${OUTCOME[u.auto] ?? u.auto})`
    case 'decides':
      return `unattended: decides \`${u.option}\` at once`
    case 'answers_default':
      return 'unattended: posts the default answer at once'
    case 'fails_run':
      return `unattended: fails the run (${u.code})`
    case 'refused_at_start':
      return `unattended: is refused at start (${u.code})`
    default:
      return 'unattended: unknown outcome (newer apb)'
  }
}

/** Whether an unattended run fails, or is refused, at this wait. */
export function unattendedStops(u: UnattendedOutcome): boolean {
  return u.kind === 'fails_run' || u.kind === 'refused_at_start'
}

/** A time in the viewer's locale, date included (tooltips, full times). */
export function localTime(ms: number): string {
  return new Date(ms).toLocaleString()
}

/** The value of a given argument (the intent id, a publish nonce). */
export function givenArg(a: NextAction, name: string): string | undefined {
  const v = a.args.find((x) => x.name === name)?.value
  return v?.kind === 'given' ? v.value : undefined
}

/** The options of a choice argument, with its default. */
export function choiceArg(a: NextAction, name: string): { options: string[]; default?: string } | null {
  const v = a.args.find((x) => x.name === name)?.value
  return v?.kind === 'choice' ? { options: v.options, default: v.default } : null
}

/** The hint of a value the person supplies (a free answer). */
export function suppliedArg(a: NextAction, name: string): string | null {
  const v = a.args.find((x) => x.name === name)?.value
  return v?.kind === 'supplied' ? v.hint : null
}

/** The consequence of a person's decision on an unknown send, as the
 * backend words it. */
export function intentConsequence(sent: boolean): string {
  return sent
    ? 'a repeat of this send answers already_sent without sending'
    : 'a repeat of this send sends it again'
}

/** How a refused start's action is met, or null when its reason says it
 * all. A remedy never carries a value or a token. */
export function remedyLine(a: StartAction): string | null {
  const r: Remedy | undefined = a.remedy
  if (r) {
    switch (r.remedy) {
      case 'approve_digest':
        return `approve the ${r.kind.replace(/_/g, ' ')} at digest ${r.digest}`
      case 'provide_param':
        return `provide a value for the param ${r.name}`
      case 'install_agent':
        return `install the ${r.agent} CLI`
      case 'set_env':
        return `set the environment variable ${r.name}`
      case 'run_doctor':
        return 'run apb doctor to see what this machine lacks'
      case 'manual':
        return r.detail
    }
  }
  if (a.code === 'confirm_irreversible_start') return 'confirm its irreversible effects when you start the run'
  if (a.code === 'acknowledge_untrusted') return 'a start from the dashboard acknowledges it'
  return null
}

/** The start actions of a refusal body, when it carries any. */
export function refusalStartActions(body: Record<string, unknown> | undefined): StartAction[] {
  const list = body?.start_actions
  return Array.isArray(list) ? (list as StartAction[]) : []
}

/** The panel a refused start leaves on the playbook page: the refusal's
 * code and the server's own explanation, with its start actions; null when
 * the refusal carries no start action. */
export function startRefusalOf(e: unknown): { message: string; actions: StartAction[] } | null {
  if (!(e instanceof ApiError)) return null
  const actions = refusalStartActions(e.body)
  if (!actions.length) return null
  const body = e.body ?? {}
  const said = [body.detail, body.reason, e.detail].find((x): x is string => typeof x === 'string' && x.trim() !== '')
  const code = e.code ? ` (${e.code})` : ''
  return { message: `The run was refused${code}${said ? `: ${said}` : '.'}`, actions }
}
