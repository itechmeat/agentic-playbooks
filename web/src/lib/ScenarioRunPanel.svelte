<script lang="ts">
  import type { AnswerRef, PlaybookScenarios, ScenarioLevel, ScenarioReport, ScenarioVerdict } from './api.gen'
  import { ApiError, fetchScenarioRun, fetchScenarios, refusalText, runScenario } from './api'
  import { authState } from './auth.svelte'
  import { SCENARIO_MAX_DECISION_REQUESTS } from './consts.gen'
  import OutcomeTag from '$lib/components/OutcomeTag.svelte'
  import { Button } from '$lib/components/ui/button'
  import type { Tone } from './runverdict'

  // The scenarios of a playbook (`evals/scenarios/<name>.yaml`) with their
  // newest result per level, and one scenario run. A quick run answers
  // every boundary from the scenario and starts nothing for real; a smoke
  // run asks the machine's decision models, so it names a request cap and
  // waits for a confirmation. The report shows the failed checks first,
  // then the trace: executions, edges taken, answers nobody used or that
  // were missing, and templates that rendered empty.
  let { id, workspace = '' }: { id: string; workspace?: string } = $props()

  // The server refuses a smoke run from a dashboard without keys (anything
  // on this machine reaches it, and smoke spends); once the status read
  // answered, smoke is not offered there.
  const authSnapshot = authState()
  const keyless = $derived(authSnapshot.checked && !authSnapshot.required)
  $effect(() => {
    if (keyless && level === 'smoke') level = 'quick'
  })
  // A refusal in the server's own sentence, without the request's address.
  const why = refusalText

  let list = $state<PlaybookScenarios | null>(null)
  let loadError = $state<string | null>(null)
  let level = $state<ScenarioLevel>('quick')
  let cap = $state(3)
  // An emptied field reads as null: the cap sent is a whole number the
  // server takes.
  const capValue = $derived(
    Number.isFinite(cap) ? Math.min(SCENARIO_MAX_DECISION_REQUESTS, Math.max(1, Math.floor(cap))) : 1,
  )
  let askSmoke = $state<string | null>(null)
  let running = $state<string | null>(null)
  let report = $state<ScenarioReport | null>(null)
  let runError = $state<string | null>(null)

  let token = 0
  async function load() {
    const t = ++token
    try {
      const r = await fetchScenarios(id, workspace)
      if (t !== token) return
      list = r
      loadError = null
    } catch (e) {
      if (t === token) loadError = String(e)
    }
  }

  $effect(() => {
    void id
    void workspace
    report = null
    runError = null
    askSmoke = null
    void load()
  })

  function run(name: string) {
    if (level === 'smoke' && askSmoke !== name) {
      askSmoke = name
      return
    }
    void start(name)
  }

  async function start(name: string) {
    askSmoke = null
    running = name
    runError = null
    try {
      const req =
        level === 'smoke'
          ? { scenario: name, level, max_decision_requests: capValue }
          : { scenario: name, level }
      report = (await runScenario(id, req, workspace)).report
    } catch (e) {
      runError =
        e instanceof ApiError && e.status === 409
          ? `The ${level} run of ${name} was refused: ${why(e)}`
          : `The ${level} run of ${name} failed: ${why(e)}`
    } finally {
      running = null
    }
    await load()
  }

  async function open(runId: string) {
    runError = null
    try {
      report = (await fetchScenarioRun(id, runId, workspace)).report
    } catch (e) {
      runError = `The result ${runId} did not load: ${why(e)}`
    }
  }

  const VERDICT_TONE: Record<ScenarioVerdict, Tone> = {
    passed: 'pass',
    failed: 'fail',
    invalid: 'gap',
    stale: 'gap',
    incomplete: 'gap',
  }

  function refText(r: AnswerRef): string {
    const parts = [r.node ?? r.kind.replace(/_/g, ' ')]
    if (r.visit) parts.push(`visit ${r.visit}`)
    if (r.item) parts.push(`item ${r.item}`)
    if (r.attempt) parts.push(`attempt ${r.attempt}`)
    if (r.part) parts.push(r.part)
    return `${parts.join(', ')} (${r.kind.replace(/_/g, ' ')})`
  }

  const checks = $derived(report ? [...report.checks].sort((a, b) => Number(a.passed) - Number(b.passed)) : [])
</script>

<div class="scenarios" data-testid="scenario-panel">
  {#if loadError}
    <p class="error">The scenarios did not load: {loadError}</p>
  {:else if !list}
    <p class="muted">Loading the scenarios</p>
  {:else}
    <div class="controls">
      <label class="field">
        <span class="muted">Level</span>
        <select
          value={level}
          onchange={(e) => {
            level = e.currentTarget.value as ScenarioLevel
            askSmoke = null
          }}
          data-testid="scenario-level"
        >
          <option value="quick">quick: scripted, nothing spent</option>
          <option value="smoke" disabled={keyless}>smoke: asks decision models</option>
        </select>
      </label>
      {#if keyless}
        <p class="muted" data-testid="scenario-smoke-keyless">
          Smoke spends decision requests, so a dashboard without keys does not start it: run
          <span class="mono">apb eval --level smoke</span> in a terminal.
        </p>
      {/if}
      {#if level === 'smoke'}
        <label class="field">
          <span class="muted">At most this many decision requests</span>
          <input type="number" min="1" max={SCENARIO_MAX_DECISION_REQUESTS} bind:value={cap} data-testid="scenario-cap" />
        </label>
      {/if}
    </div>

    {#if list.scenarios.length === 0}
      <p class="muted">
        No scenarios for {list.version}. A scenario lives in <span class="mono">evals/scenarios/</span> next to the playbook.
      </p>
    {/if}
    <ul class="list">
      {#each list.scenarios as s (s.name)}
        <li data-testid="scenario-row" data-name={s.name}>
          <div class="head" data-testid="scenario-head">
            <div class="what" data-testid="scenario-what">
              <span class="mono">{s.name}</span>
              {#if s.title}<span class="muted">{s.title}</span>{/if}
            </div>
            <Button
              variant="outline"
              size="xs"
              disabled={running !== null}
              onclick={() => run(s.name)}
              data-testid="scenario-run"
              title={level === 'quick'
                ? 'Run at quick: nothing is started for real and nothing is spent'
                : 'Run at smoke: decision models are asked, inside the cap'}
            >
              {running === s.name ? 'Running' : `Run ${level}`}
            </Button>
          </div>
          {#if s.stale}<p class="warn">Does not fit {list.version}: {s.stale}</p>{/if}
          {#each s.latest as r (r.level)}
            <div class="row latest" data-testid="scenario-latest">
              <span class="muted">{r.level}</span>
              <OutcomeTag tone={VERDICT_TONE[r.verdict]}>{r.verdict}</OutcomeTag>
              <span class="muted">{r.version}</span>
              {#if !r.fresh}
                <span class="muted" title="The result is for another digest of the playbook or of the scenario file"
                  >stale</span
                >
              {/if}
              <button type="button" class="link" onclick={() => open(r.run_id)} data-testid="scenario-open">
                {new Date(r.at_ms).toLocaleString()}
              </button>
            </div>
          {/each}
          {#if askSmoke === s.name}
            <div class="confirm" data-testid="scenario-smoke-confirm">
              <p>
                A smoke run of {s.name} sends its judge and decision questions to this machine's decision models: at most
                {capValue} decision {capValue === 1 ? 'request' : 'requests'}.
              </p>
              <div class="buttons">
                <Button variant="outline" size="xs" onclick={() => (askSmoke = null)}>Cancel</Button>
                <Button size="xs" onclick={() => start(s.name)} data-testid="scenario-smoke-go">Run smoke</Button>
              </div>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
    {#each list.issues as issue, i (i)}
      <p class="warn" data-testid="scenario-issue"><span class="mono">{issue.code}</span> {issue.message}</p>
    {/each}
  {/if}

  {#if runError}<p class="error" role="status" data-testid="scenario-error">{runError}</p>{/if}

  {#if report}
    <section class="report" data-testid="scenario-report">
      <div class="row" data-testid="scenario-verdict">
        <OutcomeTag tone={VERDICT_TONE[report.verdict]}>{report.verdict}</OutcomeTag>
        <span class="mono">{report.scenario}</span>
        <span class="muted">{report.level}, {report.version}</span>
        {#if report.run_id}<span class="muted mono">{report.run_id}</span>{/if}
      </div>
      {#if report.reason}<p class="warn">{report.reason}</p>{/if}
      {#if report.outcome || report.finish}
        <p class="muted">
          Run outcome {report.outcome ?? 'none'}{#if report.finish}, finished at <span class="mono">{report.finish}</span>{/if}
          {#if report.decision_requests}, {report.decision_requests} decision requests{/if}
          {#if report.cost_usd != null}, ${report.cost_usd.toFixed(4)}{/if}
        </p>
      {/if}

      {#if checks.length}
        <h4>Checks</h4>
        <ul class="list">
          {#each checks as c, i (i)}
            <li class="check" data-testid="scenario-check" data-passed={c.passed}>
              <div class="row">
                <OutcomeTag tone={c.passed ? 'pass' : 'fail'}>{c.passed ? 'passed' : 'failed'}</OutcomeTag>
                <span class="mono">{c.check}</span>
              </div>
              {#if !c.passed}
                <p class="detail"><span class="muted">expected</span> <span class="mono">{c.expected}</span></p>
                <p class="detail"><span class="muted">observed</span> <span class="mono">{c.observed}</span></p>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if report.executions.length}
        <h4>Executions</h4>
        <ul class="list">
          {#each report.executions as x, i (i)}
            <li class="row" data-testid="scenario-execution">
              <span class="mono">{x.node}</span>
              <span class="muted">visit {x.visit}</span>
              <span>{x.cancelled ? 'cancelled' : (x.status ?? 'open')}</span>
              {#each x.attempts ?? [] as a, j (j)}
                <span class="muted">
                  attempt {a.n}{a.item ? ` (${a.item})` : ''}: {a.status ?? 'open'}{a.failure_kind ? `, ${a.failure_kind}` : ''}{a.answer
                    ? `, answer ${a.answer}`
                    : ''}
                </span>
              {/each}
            </li>
          {/each}
        </ul>
      {/if}

      {#if report.edges_taken.length}
        <h4>Edges taken</h4>
        <ul class="list">
          {#each report.edges_taken as e (e.edge)}
            <li class="row"><span class="mono">{e.edge}</span><span class="muted">{e.count}x</span></li>
          {/each}
        </ul>
      {/if}

      {#if report.answers.extra.length}
        <h4>Answers nobody used</h4>
        <ul class="list" data-testid="scenario-unused">
          {#each report.answers.extra as r, i (i)}<li>{refText(r)}</li>{/each}
        </ul>
      {/if}
      {#if report.answers.missing.length}
        <h4>Missing answers</h4>
        <ul class="list" data-testid="scenario-missing">
          {#each report.answers.missing as r, i (i)}<li>{refText(r)}</li>{/each}
        </ul>
      {/if}
      {#if report.templates_empty.length}
        <h4>Templates that rendered empty</h4>
        <ul class="list" data-testid="scenario-empty">
          {#each report.templates_empty as t, i (i)}
            <li><span class="mono">{t.reference}</span> in {t.node}, visit {t.visit}: {t.why}</li>
          {/each}
        </ul>
      {/if}
      {#if report.joins_dead.length}
        <h4>Joins with a dead input</h4>
        <ul class="list">
          {#each report.joins_dead as j (j.join)}
            <li><span class="mono">{j.join}</span> {j.inputs.join(', ')}</li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}
</div>

<style>
  .scenarios {
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
    margin-top: 0.25rem;
    color: var(--muted-foreground);
    font-size: 0.6875rem;
    font-weight: 400;
    letter-spacing: 0.04em;
    text-transform: lowercase;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .field {
    display: flex;
    flex: 1 1 12rem;
    min-width: 0;
    flex-direction: column;
    gap: 0.25rem;
  }
  select,
  input {
    width: 100%;
    min-width: 0;
    height: 2rem;
    padding: 0 0.5rem;
    border: 1px solid var(--input, var(--border));
    border-radius: var(--radius-md, 0.375rem);
    background: transparent;
    color: var(--foreground);
    font: inherit;
  }
  select:focus-visible,
  input:focus-visible,
  .link:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .list > li[data-testid='scenario-row'] {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding-bottom: 0.375rem;
    border-bottom: 1px solid var(--border);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  /* The name and the title wrap inside their own block, so the Run button
     keeps its place at the end of the first line whatever the title. */
  .head {
    display: flex;
    align-items: flex-start;
    gap: 0.375rem;
  }
  .what {
    display: flex;
    flex: 1;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.125rem 0.375rem;
    min-width: 0;
  }
  .head > :global(button) {
    flex: none;
  }
  .latest {
    padding-left: 0.75rem;
  }
  .check {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
  }
  .detail {
    padding-left: 0.75rem;
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--foreground);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  .confirm {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding: 0.5rem;
    border: 1px solid var(--warning);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .report {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
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
