import { parse, stringify } from 'yaml'

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
