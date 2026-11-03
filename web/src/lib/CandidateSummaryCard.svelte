<script lang="ts">
  import type { AxisName, AxisState, CandidateSummary, QualityRecord, SummaryItem } from './api.gen'
  import { fetchCandidateSummary, promoteCandidate, promoteRefusalOf, runScenario } from './api'
  import OutcomeSection from '$lib/components/OutcomeSection.svelte'
  import OutcomeTag from '$lib/components/OutcomeTag.svelte'
  import { Button } from '$lib/components/ui/button'
  import type { Tone } from './runverdict'
  import { verdictText } from './quality'

  // What a person promotes, before promoting it: the candidate's fitness on
  // this machine, its definition (validator, scenario results for its exact
  // digest) and its quality, kept apart. A blocking item closes promotion;
  // attention items are listed in a confirmation, and the promotion names
  // the summary id the person acknowledged together with the exact version
  // and digest shown, so the server refuses it when anything changed since.
  let {
    id,
    workspace = '',
    frozen = false,
    refresh = 0,
    onDecided,
  }: {
    id: string
    workspace?: string
    frozen?: boolean
    // Bumped by the page when the candidate may have changed.
    refresh?: number
    onDecided: (rec: QualityRecord) => void
  } = $props()

  let summary = $state<CandidateSummary | null>(null)
  let loadError = $state<string | null>(null)
  let busy = $state(false)
  let running = $state<string | null>(null)
  // The confirmation in front of the person: the items and the id they
  // acknowledge; the server's own after a `needs_acknowledgement`.
  // It keeps the version and digest it showed, so a refresh that reads
  // another candidate never pairs this acknowledgement with that one.
  type Confirm = { version: string; digest: string; summaryId: string; items: SummaryItem[] }
  let confirm = $state<Confirm | null>(null)
  let refusal = $state<string | null>(null)
  let refusalItems = $state<SummaryItem[]>([])
  // A failed quick run, apart from the promotion's refusal.
  let runError = $state<string | null>(null)

  let token = 0
  async function load() {
    const t = ++token
    try {
      const r = await fetchCandidateSummary(id, workspace)
      if (t !== token) return
      summary = r.summary
      loadError = null
    } catch (e) {
      if (t === token) loadError = String(e)
    }
  }

  // Another playbook clears what the person was looking at; a refresh of
  // the same one only reads the summary again (the server re-checks
  // whatever the person then confirms).
  $effect(() => {
    void id
    void workspace
    confirm = null
    refusal = null
    refusalItems = []
    runError = null
  })
  $effect(() => {
    void id
    void workspace
    void refresh
    void load()
  })

  const AXES: AxisName[] = ['fitness', 'definition', 'quality']
  const AXIS_LABEL: Record<AxisName, string> = {
    fitness: 'fitness on this machine',
    definition: 'definition',
    quality: 'quality',
  }
  const STATE_TONE: Record<AxisState, Tone> = { ok: 'pass', attention: 'gap', blocking: 'fail', unknown: 'neutral' }

  function axisLine(s: CandidateSummary, axis: AxisName): string {
    if (axis === 'fitness') {
      const n = s.attention.filter((i) => i.axis === 'fitness').length
      return n ? `${n} ${n === 1 ? 'problem' : 'problems'} on this machine, never blocking` : 'runs on this machine'
    }
    if (axis === 'definition') {
      const d = s.definition.detail
      const errors = d.issues.filter((i) => i.severity === 'error').length
      const warnings = d.issues.length - errors
      const parts = [errors ? `${errors} validator ${errors === 1 ? 'error' : 'errors'}` : 'validates']
      if (warnings) parts.push(`${warnings} ${warnings === 1 ? 'warning' : 'warnings'}`)
      parts.push(`${d.scenarios.filter((r) => r.fresh).length} fresh scenario results`)
      return parts.join(', ')
    }
    const decision = s.quality.detail.view.decision
    return decision ? verdictText(decision.verdict) : 'no quality gate decides this candidate'
  }

  const short = (digest: string) => digest.replace(/^(sha256:)?(.{10}).*$/, '$1$2')

  function body(seen: { version: string; digest: string }, acknowledged?: string) {
    return {
      expected_version: seen.version,
      expected_digest: seen.digest,
      ...(acknowledged ? { acknowledged } : {}),
    }
  }

  function askPromote() {
    const s = summary
    if (!s || s.blocking.length) return
    refusal = null
    refusalItems = []
    const seen = { version: s.candidate.version, digest: s.candidate.digest }
    if (s.attention.length) confirm = { ...seen, summaryId: s.summary_id, items: s.attention }
    else void promote(seen)
  }

  async function promote(seen: { version: string; digest: string }, acknowledged?: string) {
    busy = true
    try {
      const rec = await promoteCandidate(id, body(seen, acknowledged), workspace)
      confirm = null
      onDecided(rec)
    } catch (e) {
      const r = promoteRefusalOf(e)
      confirm = null
      if (!r) {
        refusal = `The promotion failed: ${String(e)}`
      } else if (r.code === 'needs_acknowledgement') {
        refusal = 'What needs attention changed since this summary was read: look at the items again.'
        confirm = { ...seen, summaryId: r.summary_id, items: r.items }
      } else if (r.code === 'stale') {
        refusal = r.found
          ? `The candidate is now ${r.found.version} (${short(r.found.digest)}), not what this summary showed: read it again.`
          : 'There is no candidate any more.'
        await load()
      } else if (r.code === 'blocked') {
        refusal = 'The promotion is blocked:'
        refusalItems = r.items
      } else {
        refusal = 'The server asked for the version and digest this summary shows: reload the page.'
      }
    } finally {
      busy = false
    }
  }

  async function runQuick(scenario: string) {
    const s = summary
    if (!s) return
    running = scenario
    runError = null
    try {
      await runScenario(id, { scenario, level: 'quick', version: s.candidate.version }, workspace)
    } catch (e) {
      runError = `The quick run of ${scenario} failed: ${String(e)}`
    } finally {
      running = null
    }
    await load()
  }
</script>

{#if loadError}
  <p class="error" data-testid="candidate-summary-error">The candidate summary did not load: {loadError}</p>
{:else if summary}
  {@const s = summary}
  <OutcomeSection title="Before promoting" testid="candidate-summary">
    <p data-testid="summary-candidate">
      <span class="mono">{s.candidate.version}</span>
      <span class="muted mono" title={s.candidate.digest}>{short(s.candidate.digest)}</span>
      against current <span class="mono">{s.current.version}</span>
    </p>

    <ul class="axes">
      {#each AXES as axis (axis)}
        {@const state = s[axis].state}
        <li data-testid="summary-axis" data-axis={axis} data-state={state}>
          <div class="row" data-testid={`summary-axis-${axis}`}>
            <OutcomeTag tone={STATE_TONE[state]}>{state}</OutcomeTag>
            <span class="axis-name">{AXIS_LABEL[axis]}</span>
            <span class="muted">{axisLine(s, axis)}</span>
          </div>
          {#if axis === 'definition'}
            {#if s.definition.detail.scenarios.length}
              <ul class="scenarios">
                {#each s.definition.detail.scenarios as r (`${r.scenario}/${r.level}`)}
                  <li class="row" data-testid="summary-scenario" data-fresh={r.fresh}>
                    <span class="mono">{r.scenario}</span>
                    <span class="muted">{r.level}</span>
                    <OutcomeTag tone={r.verdict === 'passed' ? 'pass' : r.verdict === 'failed' ? 'fail' : 'gap'}
                      >{r.verdict}</OutcomeTag
                    >
                    {#if !r.fresh}
                      <span class="muted" title="The result is for another digest of the playbook or of the scenario file"
                        >stale</span
                      >
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
            {#each s.definition.detail.not_run as name (name)}
              <div class="row" data-testid="summary-not-run">
                <span class="mono">{name}</span>
                <span class="muted">no fresh result for this candidate</span>
                <Button
                  variant="outline"
                  size="xs"
                  disabled={running !== null || busy}
                  onclick={() => runQuick(name)}
                  data-testid="summary-run-quick"
                  title="Run this scenario at quick against the candidate: nothing is started for real and nothing is spent"
                >
                  {running === name ? 'Running' : 'Run quick'}
                </Button>
              </div>
            {/each}
          {/if}
        </li>
      {/each}
    </ul>

    {#if s.blocking.length}
      <div class="items blocking">
        <span class="items-head">Blocks a promotion</span>
        <ul>
          {#each s.blocking as item, i (i)}
            <li data-testid="summary-blocking-item"><span class="mono">{item.code}</span> {item.detail}</li>
          {/each}
        </ul>
      </div>
    {/if}
    {#if s.attention.length}
      <div class="items attention">
        <span class="items-head">To acknowledge before promoting</span>
        <ul>
          {#each s.attention as item, i (i)}
            <li data-testid="summary-attention-item"><span class="mono">{item.code}</span> {item.detail}</li>
          {/each}
        </ul>
      </div>
    {/if}

    {#if runError}
      <p class="error" role="status" data-testid="summary-run-error">{runError}</p>
    {/if}
    {#if refusal}
      <div class="refusal" role="status" data-testid="summary-refusal">
        <p>{refusal}</p>
        {#if refusalItems.length}
          <ul>
            {#each refusalItems as item, i (i)}
              <li><span class="mono">{item.code}</span> {item.detail}</li>
            {/each}
          </ul>
        {/if}
      </div>
    {/if}

    {#if confirm}
      <div class="confirm" data-testid="summary-confirm">
        <p>Promote {confirm.version} while these need attention:</p>
        <ul>
          {#each confirm.items as item, i (i)}
            <li><span class="mono">{item.code}</span> {item.detail}</li>
          {/each}
        </ul>
        <div class="buttons">
          <Button variant="outline" size="sm" disabled={busy} onclick={() => (confirm = null)}>Cancel</Button>
          <Button
            size="sm"
            disabled={busy}
            onclick={() => promote(confirm!, confirm!.summaryId)}
            data-testid="summary-confirm-promote"
          >
            Acknowledge and promote
          </Button>
        </div>
      </div>
    {:else}
      <div class="buttons">
        <Button
          size="sm"
          disabled={busy || frozen || s.blocking.length > 0}
          onclick={askPromote}
          data-testid="summary-promote"
          title={frozen
            ? 'Frozen playbook: unfreeze it to change the current version'
            : s.blocking.length
              ? 'A blocking item keeps this candidate from being promoted'
              : 'Make exactly this candidate the current version'}
        >
          Promote
        </Button>
      </div>
    {/if}
  </OutcomeSection>
{/if}

<style>
  p {
    margin: 0;
  }
  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  .axes {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .axes > li {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding-bottom: 0.375rem;
    border-bottom: 1px solid var(--border);
  }
  .axis-name {
    color: var(--foreground);
  }
  .scenarios {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    padding-left: 0.75rem;
  }
  [data-testid='summary-not-run'] {
    padding-left: 0.75rem;
  }
  .items {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.375rem 0.5rem;
    border-left: 2px solid var(--border);
  }
  .items ul,
  .confirm ul,
  .refusal ul {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
  }
  .items-head {
    color: var(--muted-foreground);
  }
  .blocking {
    border-left-color: var(--destructive);
  }
  .attention {
    border-left-color: var(--warning);
  }
  .refusal {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--warning);
    background: color-mix(in oklch, var(--warning) 10%, transparent);
  }
  .confirm {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding: 0.5rem;
    border: 1px solid var(--border);
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .error {
    color: var(--destructive);
    font-size: 0.75rem;
  }
</style>
