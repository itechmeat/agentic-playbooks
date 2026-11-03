<script lang="ts">
  import type { DecisionAnswer, JudgeOutcome, RecordedValue, SandboxEvaluation } from './api.gen'
  import { evaluateJudge, fetchJudgeRunValues, refusalText } from './api'
  import { authState } from './auth.svelte'
  import { Button } from '$lib/components/ui/button'
  import { judgeInputsOf, judgeSlots, slotValuesOf } from './judgefields'

  // One judge node evaluated outside any run: inputs typed or filled from a
  // judge execution of a run, answers given by the person (free, nothing is
  // asked) or asked of this machine's decision models (inside the sandbox's
  // daily cap). Three answers sit side by side: the raw answers, what the
  // node outputs on this machine at its mode, and what it would output at
  // enforce, each with the edges it would take. Nothing is applied: no run,
  // journal, cache or trust record is written, and no program is started.
  let {
    id,
    workspace = '',
    node,
    version,
  }: {
    id: string
    workspace?: string
    node: { id: string; state?: unknown; questions?: unknown }
    version?: string
  } = $props()

  // A choice is given by its option's name; a score by the number of its
  // level (lowest first, from 0), as the engine reads a score answer.
  type Question = { name: string; type: string; options: { value: string; label: string }[] }
  const questions = $derived.by((): Question[] => {
    const qs = node.questions
    if (!qs || typeof qs !== 'object' || Array.isArray(qs)) return []
    return Object.entries(qs as Record<string, Record<string, unknown>>).map(([name, q]) => {
      const type = typeof q?.type === 'string' ? q.type : 'choice'
      const options =
        type === 'choice' && q.criteria && typeof q.criteria === 'object'
          ? Object.keys(q.criteria).map((o) => ({ value: o, label: o }))
          : type === 'score' && Array.isArray(q.levels)
            ? (q.levels as unknown[]).map((l, i) => ({ value: String(i), label: String(l) }))
            : []
      return { name, type, options }
    })
  })
  const slots = $derived(judgeSlots(node.state))

  // A live ask spends this machine's decision requests, so a dashboard
  // without keys (anything on this machine reaches it) refuses it: once
  // the status read answered, it is not offered there.
  const authSnapshot = authState()
  const keyless = $derived(authSnapshot.checked && !authSnapshot.required)
  $effect(() => {
    if (keyless && source === 'ask') source = 'given'
  })

  // A number as recorded (a 32-bit float reads 0.8999999761581421) shown
  // as a person would write it: at most six significant digits.
  const shown = (n: number) => String(Number(n.toPrecision(6)))
  const shownOf = (x: unknown) => (typeof x === 'number' ? shown(x) : String(x))

  let values = $state<Record<string, string>>({})
  let source = $state<'given' | 'ask'>('given')
  let emulate = $state(false)
  // Given answers per question: the value (choice, score) or `p` (noul),
  // and the confidence.
  let given = $state<Record<string, { value: string; p: string; confidence: string }>>({})
  // The answers a run recorded, in full. A field shows them rounded; until
  // the person edits it, the full value is what is evaluated, so unchanged
  // recorded answers route as they did.
  let exact = $state<Record<string, { value?: unknown; p?: unknown; confidence?: unknown }>>({})
  let result = $state<SandboxEvaluation | null>(null)
  let error = $state<string | null>(null)
  let busy = $state(false)
  let runId = $state('')
  let execution = $state('')
  let recorded = $state<RecordedValue[]>([])
  let placeTarget = $state<Record<number, string>>({})

  // A reload hands the page a new node object for the same node: only
  // another node starts the sandbox afresh.
  const nodeId = $derived(node.id)
  $effect(() => {
    void nodeId
    values = {}
    given = {}
    exact = {}
    result = null
    error = null
    recorded = []
    placeTarget = {}
    // A live ask is chosen per node, never carried over to the next one.
    source = 'given'
    emulate = false
  })

  const NO_ANSWER = { value: '', p: '', confidence: '' }
  const answerOf = (name: string) => given[name] ?? NO_ANSWER
  // A person's edit: from now on the field holds what they typed, even
  // when they type the shown text of the recorded value back.
  function setAnswer(name: string, field: 'value' | 'p' | 'confidence', v: string) {
    given[name] = { ...answerOf(name), [field]: v }
    if (exact[name]?.[field] !== undefined) exact[name] = { ...exact[name], [field]: undefined }
  }

  // What a field evaluates as: the recorded value in full while the field
  // still holds it as filled and was never edited, else what was typed.
  function full(name: string, field: 'value' | 'p' | 'confidence', typed: string): unknown {
    const r = exact[name]?.[field]
    return r !== undefined && r !== null && shownOf(r) === typed ? r : typed
  }

  function answers(): Record<string, DecisionAnswer> {
    const out: Record<string, DecisionAnswer> = {}
    for (const q of questions) {
      const a = given[q.name]
      if (!a) continue
      const one: DecisionAnswer = {}
      if (q.type === 'noul') {
        if (a.p !== '') one.p = Number(full(q.name, 'p', a.p))
      } else if (a.value !== '') {
        const v = full(q.name, 'value', a.value)
        one.value = q.type === 'score' ? Number(v) : String(v)
      }
      if (a.confidence !== '') one.confidence = Number(full(q.name, 'confidence', a.confidence))
      if (Object.keys(one).length) out[q.name] = one
    }
    return out
  }

  async function evaluate() {
    busy = true
    error = null
    try {
      const inputs = judgeInputsOf(slots, values)
      const req =
        source === 'given'
          ? { ...(version ? { version } : {}), inputs, answers: answers(), emulate: false }
          : { ...(version ? { version } : {}), inputs, emulate }
      result = await evaluateJudge(id, node.id, req, workspace)
    } catch (e) {
      error = refusalText(e)
    } finally {
      busy = false
    }
  }

  async function fill() {
    if (!runId.trim()) return
    busy = true
    error = null
    try {
      const n = Number(execution)
      const v = await fetchJudgeRunValues(runId.trim(), node.id, execution && n > 0 ? n : undefined, workspace)
      values = slotValuesOf(slots, v.inputs)
      recorded = v.recorded
      placeTarget = {}
      // The answers go with the inputs they were given for: an execution
      // without observed answers leaves none.
      const g: typeof given = {}
      const e: typeof exact = {}
      if (v.observed) {
        for (const [name, a] of Object.entries(v.observed.answers)) {
          e[name] = { value: a.value, p: a.p, confidence: a.confidence }
          g[name] = {
            value: a.value === undefined || a.value === null ? '' : shownOf(a.value),
            p: a.p === undefined || a.p === null ? '' : shown(a.p),
            confidence: a.confidence === undefined || a.confidence === null ? '' : shown(a.confidence),
          }
        }
        source = 'given'
      }
      given = g
      exact = e
    } catch (e) {
      error = refusalText(e)
    } finally {
      busy = false
    }
  }

  function place(i: number) {
    const key = placeTarget[i] ?? slots[0]?.key
    if (key) values[key] = recorded[i].text
  }

  function rawText(a: DecisionAnswer): string {
    if (a.invalid) return `invalid: ${a.invalid}`
    const parts: string[] = []
    if (a.value !== undefined && a.value !== null) parts.push(String(a.value))
    if (a.p !== undefined) parts.push(`p ${a.p.toFixed(2)}`)
    if (a.confidence !== undefined) parts.push(`confidence ${a.confidence.toFixed(2)}`)
    return parts.join(', ') || 'no answer'
  }

  function outcomeText(o: JudgeOutcome): string {
    const out = (v: Record<string, unknown>) =>
      Object.entries(v)
        .map(([k, x]) => `${k} ${typeof x === 'string' ? x : typeof x === 'number' ? shown(x) : JSON.stringify(x)}`)
        .join(', ')
    if (o.kind === 'output') return out(o.output)
    if (o.kind === 'fallback') return `fallback (${o.reason}): ${out(o.output)}`
    if (o.kind === 'fail') return `fails: ${o.reason}`
    return `emulates: ${o.reason}`
  }

  const unfilled = $derived(result ? result.evaluation.fields.flatMap((f) => f.unfilled ?? []) : [])
</script>

<div class="sandbox" data-testid="judge-sandbox">
  <p class="muted">
    Evaluates <span class="mono">{node.id}</span> outside any run: no run, journal or trust record is written, and no
    program is started.
  </p>

  <fieldset>
    <legend>Inputs the state reads</legend>
    {#if slots.length === 0}
      <p class="muted">The state reads no run value: it renders the same every time.</p>
    {/if}
    {#each slots as slot (slot.key)}
      <label class="field" data-testid={`sandbox-slot-${slot.key}`}>
        <span class="mono">{slot.refs.join(', ')}</span>
        <textarea
          rows="2"
          value={values[slot.key] ?? ''}
          oninput={(e) => (values[slot.key] = e.currentTarget.value)}
          data-testid={`sandbox-input-${slot.key}`}
          data-key={slot.key}
        ></textarea>
        {#if !(values[slot.key] ?? '')}<span class="warn">empty: renders as nothing</span>{/if}
      </label>
    {/each}
    <div class="row">
      <input
        type="text"
        placeholder="run id"
        value={runId}
        oninput={(e) => (runId = e.currentTarget.value)}
        data-testid="sandbox-run-id"
      />
      <input
        type="number"
        min="1"
        placeholder="execution"
        title="The judge execution, 1-based; the latest when empty"
        value={execution}
        oninput={(e) => (execution = e.currentTarget.value)}
        data-testid="sandbox-execution"
      />
      <Button variant="outline" size="xs" disabled={busy || !runId.trim()} onclick={fill} data-testid="sandbox-fill">
        Fill from the run
      </Button>
    </div>
    {#if recorded.length}
      <ul class="list">
        {#each recorded as r, i (i)}
          <li class="row">
            <span class="mono">{r.label}</span>
            <span class="clip" title={r.text}>{r.text}</span>
            <select
              value={placeTarget[i] ?? slots[0]?.key ?? ''}
              onchange={(e) => (placeTarget[i] = e.currentTarget.value)}
              data-testid={`sandbox-place-target-${i}`}
            >
              {#each slots as s (s.key)}<option value={s.key}>{s.refs.join(', ')}</option>{/each}
            </select>
            <Button variant="outline" size="xs" onclick={() => place(i)} data-testid={`sandbox-place-${i}`}>Use</Button>
          </li>
        {/each}
      </ul>
    {/if}
  </fieldset>

  <fieldset>
    <legend>Answers</legend>
    <label class="choice">
      <input
        type="radio"
        name="sandbox-source"
        checked={source === 'given'}
        onchange={() => (source = 'given')}
        data-testid="sandbox-source-given"
      />
      Give the answers: nothing is asked or spent
    </label>
    <label class="choice">
      <input
        type="radio"
        name="sandbox-source"
        checked={source === 'ask'}
        disabled={keyless}
        onchange={() => (source = 'ask')}
        data-testid="sandbox-source-ask"
      />
      Ask this machine's decision models (counts toward the sandbox's daily cap)
    </label>
    {#if keyless}
      <p class="muted" data-testid="sandbox-ask-keyless">
        A dashboard without keys does not ask decision models: give the answers here, or run
        <span class="mono">apb judge eval</span> in a terminal.
      </p>
    {/if}
    {#if source === 'ask'}
      <label class="choice">
        <input
          type="checkbox"
          checked={emulate}
          onchange={(e) => (emulate = e.currentTarget.checked)}
          data-testid="sandbox-emulate"
        />
        Ask the emulation providers instead (priced; the node's profile is never started)
      </label>
    {:else}
      {#each questions as qn (qn.name)}
        {@const a = answerOf(qn.name)}
        <div class="row answer">
          <span class="mono">{qn.name}</span>
          <span class="muted">{qn.type}</span>
          {#if qn.type === 'noul'}
            <input
              type="number"
              min="0"
              max="1"
              step="0.01"
              placeholder="p of yes"
              value={a.p}
              oninput={(e) => setAnswer(qn.name, 'p', e.currentTarget.value)}
              data-testid={`sandbox-p-${qn.name}`}
            />
          {:else}
            <select
              value={a.value}
              onchange={(e) => setAnswer(qn.name, 'value', e.currentTarget.value)}
              data-testid={`sandbox-answer-${qn.name}`}
            >
              <option value="">no answer</option>
              {#each qn.options as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
              {#if a.value !== '' && !qn.options.some((o) => o.value === a.value)}
                <!-- An observed score: the expected level, between two levels. -->
                <option value={a.value}>level {a.value} (observed)</option>
              {/if}
            </select>
            <input
              type="number"
              min="0"
              max="1"
              step="0.01"
              placeholder="confidence"
              value={a.confidence}
              oninput={(e) => setAnswer(qn.name, 'confidence', e.currentTarget.value)}
              data-testid={`sandbox-confidence-${qn.name}`}
            />
          {/if}
        </div>
      {/each}
    {/if}
  </fieldset>

  <div class="buttons">
    <Button size="sm" disabled={busy} onclick={evaluate} data-testid="sandbox-evaluate">
      {busy ? 'Evaluating' : 'Evaluate'}
    </Button>
  </div>

  {#if error}<p class="error" role="status" data-testid="sandbox-error">{error}</p>{/if}

  {#if result}
    {@const ev = result.evaluation}
    <section class="result" data-testid="sandbox-result">
      <div class="grid">
        <div class="cell" data-testid="sandbox-raw">
          <h4>Raw answers</h4>
          {#each Object.entries(ev.raw) as [name, a] (name)}
            <p><span class="mono">{name}</span> {rawText(a)}</p>
          {:else}
            <p class="muted">no answer</p>
          {/each}
        </div>
        <div class="cell" data-testid="sandbox-as-configured">
          <h4>On this machine ({ev.mode ?? 'not configured'})</h4>
          <p>{outcomeText(ev.as_configured)}</p>
          <p class="muted" data-testid="sandbox-route-as-configured">
            route {result.route_as_configured.join(', ') || 'none'}
          </p>
        </div>
        <div class="cell" data-testid="sandbox-at-enforce">
          <h4>At enforce</h4>
          <p>{outcomeText(ev.at_enforce)}</p>
          <p class="muted" data-testid="sandbox-route-at-enforce">route {result.route_at_enforce.join(', ') || 'none'}</p>
        </div>
      </div>
      {#if ev.would_change}<p class="warn">Applying the answers would change what the node outputs here.</p>{/if}
      <p class="muted" data-testid="sandbox-route-note">{result.route_note}</p>
      {#if unfilled.length}
        <p class="warn" data-testid="sandbox-unfilled">Rendered as nothing: {unfilled.join(', ')}</p>
      {/if}
      <p class="muted" data-testid="sandbox-spend">
        {ev.requests}
        {ev.requests === 1 ? 'request' : 'requests'}{#if ev.cost_usd != null}, ${ev.cost_usd.toFixed(4)}{/if}; sandbox
        requests of this project today: {result.requests_today} of {result.daily_cap}
      </p>
    </section>
  {/if}
</div>

<style>
  .sandbox {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }
  p,
  h4 {
    margin: 0;
  }
  h4 {
    color: var(--muted-foreground);
    font-size: 0.6875rem;
    font-weight: 400;
    letter-spacing: 0.04em;
    text-transform: lowercase;
  }
  fieldset {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    min-width: 0;
    margin: 0;
    padding: 0.5rem;
    border: 1px solid var(--border);
  }
  legend {
    padding: 0 0.25rem;
    color: var(--muted-foreground);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  textarea,
  input[type='text'],
  input[type='number'],
  select {
    min-width: 0;
    padding: 0.25rem 0.5rem;
    border: 1px solid var(--input, var(--border));
    border-radius: var(--radius-md, 0.375rem);
    background: transparent;
    color: var(--foreground);
    font: inherit;
  }
  input[type='number'] {
    width: 7rem;
  }
  input[type='text'] {
    flex: 1 1 8rem;
  }
  textarea {
    resize: vertical;
  }
  textarea:focus-visible,
  input:focus-visible,
  select:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  .choice {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: start;
    gap: 0.375rem;
  }
  .choice input {
    margin: 0.15rem 0 0;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .clip {
    flex: 1 1 8rem;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
  }
  .result {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
    gap: 0.5rem;
  }
  .cell {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--border);
  }
  .muted {
    color: var(--muted-foreground);
  }
  .warn {
    color: var(--warning);
  }
  .error {
    color: var(--destructive);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
</style>
