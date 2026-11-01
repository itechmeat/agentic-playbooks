// The launch policy refusals the dashboard answers itself.
//
// A dashboard without keys counts as the CLI without a terminal: an `ask`
// playbook starts (or a never-authorized run resumes) only with the
// `launch_nonce` of its 409 refusal, sent back with the refusal's
// `launch_version` and `launch_choice`; a `deny` one only from a terminal or
// a dashboard with keys. The editor lowers a launch floor only after the
// person confirmed that exact lowering (with keys), and never without keys.

/** What a confirmed retry of a launch sends back. */
export interface LaunchConfirmation {
  nonce: string
  version?: string
  choice?: string
}

export type LaunchStep =
  | { kind: 'confirm'; message: string; confirmation: LaunchConfirmation }
  | { kind: 'person'; message: string }

const str = (v: unknown): string | undefined => (typeof v === 'string' && v ? v : undefined)

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
  if (!policy || !policy.startsWith('launch_')) return null
  const message = str(body.detail) ?? policy
  const nonce = str(body.launch_nonce)
  if (nonce && nonce !== sent?.nonce) {
    return {
      kind: 'confirm',
      message,
      confirmation: {
        nonce,
        version: str(body.launch_version),
        choice: str(body.launch_choice),
      },
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

/** The question asked before a confirmed launch. */
export function launchQuestion(step: { message: string }): string {
  return `${step.message}\n\nLaunch it now?`
}
