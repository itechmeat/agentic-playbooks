<script lang="ts" module>
  /** The attempt the run is on (or ended on), for the head of the box. */
  export interface CurrentAttempt {
    node: string
    profile?: string
    agent?: string
    model?: string
    /** No model reported: the model the attempt was asked for, a hint only. */
    modelHint?: string
    attempt?: number
    progress?: number
    duration?: string
    status?: string
    /** Recent lines about the attempt (summary, transcript path), oldest first. */
    tail: string[]
  }
</script>

<script lang="ts">
  import type { ExecutionIdentityView, HintFollow } from './api.gen'
  import { hintFollowText } from './playbookstats'
  import { profileColor, statusColor } from './profilecolor'
  import { shortModel } from './format'
  import { attemptModelText, attemptStepText } from './runview'

  // The attempt box: the attempt the run is on (agent, model, attempt n, the
  // recent lines about it), then each attempt's identity: the model apb
  // requested of a CLI, or what a host reported (one, mixed, partial, not
  // reported) with the hint kept apart, its chain position and why its step
  // was entered. In host mode the profile's model is only a hint: the host
  // picks its own, and a run whose nodes ran on another model than the
  // profile names says so here.
  let {
    models,
    hintFollow = null,
    current = null,
  }: {
    models?: ExecutionIdentityView[] | null
    /** How often the host followed apb's model hints in this run. */
    hintFollow?: HintFollow | null
    current?: CurrentAttempt | null
  } = $props()

  const rows = $derived(models ?? [])
  const mismatches = $derived(rows.filter((a) => a.mismatch).length)
</script>

{#if rows.length || current}
  <section class="box panel" style:--k="var(--c-blue)" data-testid="run-models">
    <span class="box-cap">attempt</span>
    {#if current}
      <div class="current-head" data-testid="run-current-attempt">
        <span class="current-node">{current.node}</span>
        {#if current.profile}
          <span class="current-profile" style:color={profileColor(current.profile)}>{current.profile}</span>
        {/if}
      </div>
      <div class="current-line" title={current.model ?? current.modelHint}>
        {[
          current.agent,
          current.model
            ? shortModel(current.model)
            : current.modelHint
              ? `model ${current.status === 'running' ? 'awaiting report' : 'not reported'} (hint ${shortModel(current.modelHint)})`
              : '',
          current.attempt ? `attempt ${current.attempt}` : '',
          current.duration,
        ]
          .filter(Boolean)
          .join(' · ')}
        {#if current.progress !== undefined}· {current.progress}%{/if}
        {#if current.status}· <span style:color={statusColor(current.status)}>{current.status}</span>{/if}
      </div>
      {#if current.tail.length}
        <pre class="tail">{#each current.tail as line, i (i)}<span class="tail-mark">&gt; </span>{line}{'\n'}{/each}</pre>
      {/if}
    {/if}
    {#if rows.length}
      <div class="models-head">
        models
        {#if mismatches}
          <span class="tag mismatch" data-testid="run-models-mismatch">
            {mismatches} differ from the profile
          </span>
        {/if}
      </div>
      <div class="rows">
        {#each rows as a, i (i)}
          <div class="row" data-testid="run-model-row">
            <span class="row-node">{a.node}{a.item ? ` [${a.item}]` : ''}</span>
            <span class="muted">#{a.attempt}</span>
            <span class="row-model">{attemptModelText(a)}</span>
            <span class="muted">· {a.executed_by}</span>
            {#if a.mismatch}
              <span class="tag mismatch" title={`The profile names ${a.expected}`}>profile: {a.expected}</span>
            {/if}
            <span class="row-step" class:muted={!a.step}>{attemptStepText(a)}</span>
          </div>
        {/each}
      </div>
      {#if hintFollow}
        <div class="hint-follow" data-testid="run-models-hints">{hintFollowText(hintFollow)}</div>
      {/if}
    {/if}
  </section>
{/if}

<style>
  .panel {
    padding: 1rem 1rem 0.75rem;
  }
  .current-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .current-node {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .current-profile {
    flex-shrink: 0;
    font-size: 0.75rem;
  }
  .current-line,
  .muted {
    color: var(--muted-foreground);
  }
  .current-line {
    font-size: 0.75rem;
  }
  .tail {
    max-height: 10rem;
    margin: 0.5rem 0 0;
    padding: 0.5rem 0.625rem;
    overflow: auto;
    border: 1px solid var(--c-line);
    background: var(--c-field);
    color: var(--c-fg-2);
    font-size: 11.5px;
    overflow-wrap: break-word;
    white-space: pre-wrap;
  }
  .tail-mark {
    color: var(--c-blue);
  }
  .models-head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.75rem;
    color: var(--muted-foreground);
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .mismatch {
    color: var(--warning);
    text-transform: none;
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    max-height: 12rem;
    margin-top: 0.25rem;
    overflow: auto;
    font-size: 0.75rem;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    column-gap: 0.375rem;
  }
  .row-node {
    color: var(--foreground);
  }
  .row-model {
    color: var(--c-fg-2);
    overflow-wrap: anywhere;
  }
  .row-step {
    flex-basis: 100%;
    padding-left: 0.75rem;
    color: var(--c-fg-2);
    overflow-wrap: anywhere;
  }
  .hint-follow {
    margin-top: 0.25rem;
    color: var(--muted-foreground);
    font-size: 0.75rem;
  }
  .row-step.muted {
    color: var(--muted-foreground);
  }
</style>
