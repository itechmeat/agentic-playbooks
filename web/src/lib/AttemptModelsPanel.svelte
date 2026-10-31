<script lang="ts" module>
  /** The attempt the run is on (or ended on), for the head of the box. */
  export interface CurrentAttempt {
    node: string
    profile?: string
    agent?: string
    model?: string
    attempt?: number
    progress?: number
    duration?: string
    status?: string
    /** Recent lines about the attempt (summary, transcript path), oldest first. */
    tail: string[]
  }
</script>

<script lang="ts">
  import type { AttemptModel } from './api.gen'
  import { profileColor, statusColor } from './profilecolor'
  import { shortModel } from './format'

  // The attempt box: the attempt the run is on (agent, model, attempt n, the
  // recent lines about it), then the model each attempt actually ran on
  // (issue #193). In host mode the profile's model is only a hint: the host
  // picks its own, and a run whose nodes all ran on another model than the
  // profile names says so here.
  let { models, current = null }: { models?: AttemptModel[] | null; current?: CurrentAttempt | null } =
    $props()

  const rows = $derived(models ?? [])
  const mismatches = $derived(rows.filter((a) => a.mismatch).length)
</script>

{#if rows.length || current}
  <section class="box px-4 pt-4 pb-3" style:--k="var(--c-blue)" data-testid="run-models">
    <span class="box-cap">attempt</span>
    {#if current}
      <div class="flex items-baseline justify-between gap-2" data-testid="run-current-attempt">
        <span class="min-w-0 truncate font-semibold text-foreground">{current.node}</span>
        {#if current.profile}
          <span class="shrink-0 text-xs" style:color={profileColor(current.profile)}>{current.profile}</span>
        {/if}
      </div>
      <div class="text-xs text-muted-foreground" title={current.model}>
        {[current.agent, current.model ? shortModel(current.model) : '', current.attempt ? `attempt ${current.attempt}` : '', current.duration]
          .filter(Boolean)
          .join(' · ')}
        {#if current.progress !== undefined}· {current.progress}%{/if}
        {#if current.status}· <span style:color={statusColor(current.status)}>{current.status}</span>{/if}
      </div>
      {#if current.tail.length}
        <pre
          class="mt-2 mb-0 max-h-40 overflow-auto border border-line bg-field px-2.5 py-2 text-[11.5px] break-words whitespace-pre-wrap text-fg-2">{#each current.tail as line, i (i)}<span
              class="text-blue">&gt; </span>{line}{'\n'}{/each}</pre>
      {/if}
    {/if}
    {#if rows.length}
    <div class="mt-3 flex items-center gap-2 text-[11px] tracking-[0.08em] text-muted-foreground uppercase">
      models
      {#if mismatches}
        <span class="tag text-warning normal-case" data-testid="run-models-mismatch">
          {mismatches} differ from the profile
        </span>
      {/if}
    </div>
    <div class="mt-1 flex max-h-48 flex-col gap-0.5 overflow-auto text-xs">
      {#each rows as a, i (i)}
        <div class="flex flex-wrap items-center gap-x-1.5" data-testid="run-model-row">
          <span class="text-foreground">{a.node}</span>
          <span class="text-muted-foreground">#{a.attempt}</span>
          <span class="break-all text-fg-2">{a.model ?? 'not reported'}</span>
          <span class="text-muted-foreground">· {a.executed_by}</span>
          {#if a.mismatch}
            <span class="tag text-warning" title={`The profile names ${a.expected}`}>profile: {a.expected}</span>
          {/if}
        </div>
      {/each}
    </div>
    {/if}
  </section>
{/if}
