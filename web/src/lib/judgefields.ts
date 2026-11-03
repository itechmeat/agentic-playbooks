import { parse, stringify } from 'yaml'
import type { JudgeInputs } from './api.gen'

// The judge node's structured fields (state, questions, thresholds,
// on_unavailable) are edited as small YAML snippets in the node form: the
// question editors of a decision model are nested maps (options with their
// criteria, levels, bands), and a snippet keeps their order and comments
// exactly as the author wrote them in the playbook.

/** The YAML snippet shown for a field value; empty when the field is absent. */
export function toYamlField(value: unknown): string {
  if (value === undefined || value === null) return ''
  if (typeof value === 'string') return value
  return stringify(value).trimEnd()
}

export type YamlFieldResult = { ok: true; value: unknown } | { ok: false; error: string }

/**
 * Parses a snippet back into a field value. Empty text removes the field
 * (`undefined`); text that does not parse is an error and writes nothing,
 * so a half-typed snippet never lands in the playbook.
 */
export function fromYamlField(text: string): YamlFieldResult {
  if (text.trim() === '') return { ok: true, value: undefined }
  try {
    return { ok: true, value: parse(text) }
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message.split('\n')[0] : String(e) }
  }
}

/**
 * A one-line summary of a judge node's questions for the graph and the form
 * header: `verdict (choice), risky (noul)`. A list-shaped value (which the
 * validator refuses) is summarized too, so the canvas still says something.
 */
export function questionSummary(questions: unknown): string {
  const entries: [string, unknown][] = Array.isArray(questions)
    ? questions.map((q, i) => [String((q as { id?: unknown })?.id ?? `q${i + 1}`), q])
    : questions && typeof questions === 'object'
      ? Object.entries(questions as Record<string, unknown>)
      : []
  return entries
    .map(([id, q]) => {
      const t = (q as { type?: unknown } | null)?.type
      return typeof t === 'string' ? `${id} (${t})` : id
    })
    .join(', ')
}

/** The starter fields of a new judge node: valid once its state is filled. */
export function judgeSkeleton(): Record<string, unknown> {
  return {
    state: { input: '{{run.instruction}}' },
    questions: {
      verdict: {
        type: 'choice',
        instructions: 'Which outcome does `input` describe?',
        criteria: {
          ok: 'The work is done as requested.',
          needs_work: 'At least one concrete problem must be fixed first.',
          unclear: 'Too short, cut off or unrelated to tell.',
        },
      },
    },
    thresholds: { verdict: { min_confidence: 0.6, below: 'unclear' } },
    on_unavailable: 'fail',
  }
}

/**
 * One input of a judge node's state the sandbox asks for: where the engine
 * reads it in `JudgeInputs` (`key`, as `instruction`, `context`,
 * `params.<name>`, `outputs.<node>`, `reviews.<node>.note`,
 * `reviews.<node>.decision`, `rejected_outputs.<node>`, `hooks.<name>`)
 * and the template references that read it.
 */
export interface JudgeSlot {
  key: string
  refs: string[]
}

// Where one reference reads (`apb_engine::context::resolve`); null for what
// a run fills by itself (its id, its retro, its elapsed time).
function slotKey(ref: string): string | null {
  const p = ref.split('.')
  if (p[0] === 'params' && p.length === 2) return `params.${p[1]}`
  if (ref === 'run.instruction') return 'instruction'
  if (ref === 'run.context') return 'context'
  if (p[0] === 'run' && p[1] === 'hooks' && p.length === 3) return `hooks.${p[2]}`
  if (p[0] !== 'nodes' || p.length < 3) return null
  const [, node, what] = p
  if ((what === 'output' || what === 'report') && p.length <= 4) return `outputs.${node}`
  if (p.length !== 3) return null
  if (what === 'review_note') return `reviews.${node}.note`
  if (what === 'review_decision') return `reviews.${node}.decision`
  if (what === 'rejected_output') return `rejected_outputs.${node}`
  return null
}

// The `{{ ... }}` references of a text, as `apb_core::template::refs` reads them.
function templateRefs(text: string): string[] {
  const out: string[] = []
  const re = /\{\{([\s\S]*?)\}\}/g
  for (let m = re.exec(text); m; m = re.exec(text)) out.push(m[1].trim())
  return out
}

function strings(v: unknown, out: string[] = []): string[] {
  if (typeof v === 'string') out.push(v)
  else if (Array.isArray(v)) v.forEach((x) => strings(x, out))
  else if (v && typeof v === 'object') Object.values(v).forEach((x) => strings(x, out))
  return out
}

/** The inputs a judge node's `state` reads, each once, in reading order. */
export function judgeSlots(state: unknown): JudgeSlot[] {
  const slots: JudgeSlot[] = []
  for (const text of strings(state)) {
    for (const ref of templateRefs(text)) {
      const key = slotKey(ref)
      if (!key) continue
      const slot = slots.find((s) => s.key === key)
      if (!slot) slots.push({ key, refs: [ref] })
      else if (!slot.refs.includes(ref)) slot.refs.push(ref)
    }
  }
  return slots
}

/** The `JudgeInputs` a sandbox request carries for the typed values. */
export function judgeInputsOf(slots: JudgeSlot[], values: Record<string, string>): JudgeInputs {
  const inputs: JudgeInputs = {
    params: {},
    instruction: null,
    outputs: {},
    reviews: {},
    rejected_outputs: {},
    hooks: {},
    context: '',
  }
  for (const { key } of slots) {
    const v = values[key] ?? ''
    const [kind, name, part] = key.split('.')
    if (kind === 'instruction') inputs.instruction = v
    else if (kind === 'context') inputs.context = v
    else if (kind === 'reviews') {
      const r = (inputs.reviews[name] ??= { decision: '', note: null })
      if (part === 'note') r.note = v
      else r.decision = v
    } else if (kind === 'params' || kind === 'outputs' || kind === 'rejected_outputs' || kind === 'hooks') {
      inputs[kind][name] = v
    }
  }
  return inputs
}

/** The value of each input in recorded `JudgeInputs` (a run's, an example's). */
export function slotValuesOf(slots: JudgeSlot[], inputs: JudgeInputs): Record<string, string> {
  const out: Record<string, string> = {}
  for (const { key } of slots) {
    const [kind, name, part] = key.split('.')
    if (kind === 'instruction') out[key] = inputs.instruction ?? ''
    else if (kind === 'context') out[key] = inputs.context
    else if (kind === 'reviews') {
      const r = inputs.reviews[name]
      out[key] = (part === 'note' ? r?.note : r?.decision) ?? ''
    } else if (kind === 'params' || kind === 'outputs' || kind === 'rejected_outputs' || kind === 'hooks') {
      out[key] = inputs[kind][name] ?? ''
    }
  }
  return out
}
