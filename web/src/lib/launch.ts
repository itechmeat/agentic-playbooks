// The launch policy refusals the dashboard answers itself.
//
// A dashboard without keys counts as the CLI without a terminal: an `ask`
// playbook starts (or a never-authorized run resumes) only with the
// `launch_nonce` of its 409 refusal, sent back with the refusal's
// `launch_version` and `launch_choice`; a `deny` one only from a terminal or
// a dashboard with keys. Agent work that is billed, or whose basis is not
// known, starts only after the person confirmed it here, keys or not (a
// `billing_` refusal with its nonce). The editor lowers a launch floor only
// after the person confirmed that exact lowering (with keys), and never
// without keys.

/** What a confirmed retry of a launch sends back. */
export interface LaunchConfirmation {
  nonce: string
  version?: string
  choice?: string
}

/** The billed use a refusal holds: the agents and bases, and the settings. */
export interface HeldBilling {
  held: string[]
  sources: string[]
}

export type LaunchStep =
  | { kind: 'confirm'; message: string; confirmation: LaunchConfirmation; billing?: HeldBilling }
  | { kind: 'person'; message: string }

const str = (v: unknown): string | undefined => (typeof v === 'string' && v ? v : undefined)

const strs = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string' && x !== '') : []

/** The `billing` block of a refusal, when it holds any agent. */
function heldBilling(v: unknown): HeldBilling | undefined {
  if (!v || typeof v !== 'object') return undefined
  const b = v as Record<string, unknown>
  const held = strs(b.held)
  return held.length ? { held, sources: strs(b.sources) } : undefined
}

/**
 * The next step for a refused start or resume: a confirmation the person can
 * give here (the refusal carries a nonce), or a launch only a person at a
 * terminal makes. `null` for a refusal that is not the launch policy's, or a
 * retry whose nonce was refused again (`sent`), which is not asked twice.
 */
export function launchStep(
  body: Record<string, unknown> | undefined,
  sent?: LaunchConfirmation,
): LaunchStep | null {
  if (!body) return null
  const policy = str(body.policy)
  if (!policy || !(policy.startsWith('launch_') || policy.startsWith('billing_'))) return null
  const message = str(body.detail) ?? policy
  const nonce = str(body.launch_nonce)
  if (nonce && nonce !== sent?.nonce) {
    const billing = heldBilling(body.billing)
    return {
      kind: 'confirm',
      message,
      confirmation: {
        nonce,
        version: str(body.launch_version),
        choice: str(body.launch_choice),
      },
      ...(billing ? { billing } : {}),
    }
  }
  return { kind: 'person', message }
}

/** The fields a confirmed start sends. */
export function launchFields(c: LaunchConfirmation | undefined): Record<string, string> {
  if (!c) return {}
  return {
    confirm_launch: c.nonce,
    ...(c.version ? { launch_version: c.version } : {}),
    ...(c.choice ? { launch_choice: c.choice } : {}),
  }
}

export type FloorStep =
  | { kind: 'confirm'; message: string; declared: string; floor: string }
  | { kind: 'terminal'; message: string }

/**
 * The next step for a refused editor save: a lowering of the launch floor
 * the person can confirm (a dashboard with keys), or one only `apb launch
 * floor --lower-to` at a terminal makes. `null` for any other refusal.
 */
export function floorStep(
  code: string | undefined,
  body: Record<string, unknown> | undefined,
): FloorStep | null {
  const message = str(body?.message) ?? code ?? ''
  if (code === 'launch_floor_lowering_needs_terminal') return { kind: 'terminal', message }
  if (code !== 'launch_floor_lowering_unconfirmed') return null
  const declared = str(body?.declared)
  const floor = str(body?.floor)
  if (!declared || !floor) return { kind: 'terminal', message }
  return { kind: 'confirm', message, declared, floor }
}

/** The question the editor asks before it lowers a floor. */
export function floorQuestion(step: { declared: string; floor: string }): string {
  return `This version lowers the launch floor from ${step.floor} to ${step.declared}: agents may then launch it as ${step.declared}. Lower it?`
}

/** What the confirmation dialog shows a person, in their words. */
export interface LaunchQuestion {
  title: string
  /** Why a person is asked, without the instructions meant for an agent. */
  reason: string
  /** The agents and bases that would be billed or are of unknown basis. */
  held: string[]
  /** The settings that ask for the confirmation. */
  sources: string[]
}

/** The question asked before a confirmed launch. */
export function launchQuestion(step: { message: string; billing?: HeldBilling }): LaunchQuestion {
  // A refusal's detail ends with what an agent does next (`; ask the
  // person ... call again with confirm_launch ...`): the person is shown
  // only the part before it.
  const reason = step.message.split(/;\s*ask the person/)[0].trim()
  if (step.billing) {
    return {
      title: 'Start billed agent work?',
      reason:
        'This run would start agent work that is billed, or whose cost basis is not known. Your billing settings ask you first.',
      held: step.billing.held,
      sources: step.billing.sources.map((s) => (s === 'default' ? 'the built-in default' : s)),
    }
  }
  return { title: 'Launch this playbook?', reason, held: [], sources: [] }
}
