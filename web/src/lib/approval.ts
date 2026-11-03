// Approval refusals on the run page (PR 20b): a node with
// `requires_approval` that its check refused finishes failed with the
// output `<code>: <detail>` and no attempt, and a gate whose bound snapshot
// stopped verifying when its decision was consumed ends the run with the
// same text as the failure reason. Pure helpers; the card shows the result.

import type { ApprovalRefusal } from './api.gen'
import { APPROVAL_REFUSAL_TEXTS, APPROVAL_REFUSALS } from './consts.gen'
import type { RunDetail } from './types'

// The codes and their sentences come from the Rust side (`ts_contract`):
// `apb_engine::approval::ApprovalRefusal` and its `sentence`.
export type { ApprovalRefusal }

// Every code the server writes is one the generated union knows, and the
// sentence map covers exactly that list.
APPROVAL_REFUSALS satisfies readonly ApprovalRefusal[]
APPROVAL_REFUSAL_TEXTS satisfies Record<ApprovalRefusal, string>

export interface ApprovalRefusalView {
  code: ApprovalRefusal
  /** The sentence for the code. */
  text: string
  /** What the engine said it is about (a node, a path, a snapshot id). */
  detail: string
}

const isRefusal = (code: string): code is ApprovalRefusal => Object.hasOwn(APPROVAL_REFUSAL_TEXTS, code)

/** The refusal a node's output starts with (`<code>: <detail>`), or null. */
export function approvalRefusal(output: string | null | undefined): ApprovalRefusalView | null {
  const m = /^(approval_[a-z_]+)(?::\s*([\s\S]*))?$/.exec(output ?? '')
  if (!m || !isRefusal(m[1])) return null
  return { code: m[1], text: APPROVAL_REFUSAL_TEXTS[m[1]], detail: (m[2] ?? '').trim() }
}

/** Every node of the run its approval check refused: a failed node whose
 * output is a refusal, and the gate named by a run failure that is one. */
export function nodeApprovalRefusals(
  detail: Pick<RunDetail, 'nodes' | 'outputs' | 'failure_reason'>,
): Map<string, ApprovalRefusalView> {
  const out = new Map<string, ApprovalRefusalView>()
  for (const [node, status] of Object.entries(detail.nodes ?? {})) {
    if (status !== 'failed') continue
    const r = approvalRefusal(detail.outputs?.[node])
    if (r) out.set(node, r)
  }
  const m = /^node `([^`]+)`: ([\s\S]*)$/.exec(detail.failure_reason ?? '')
  const r = m ? approvalRefusal(m[2]) : null
  if (m && r && !out.has(m[1])) out.set(m[1], r)
  return out
}
