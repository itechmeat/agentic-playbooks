<script lang="ts">
  // "Notify me" on a run (a strip under the run's title): whether the run
  // tells a person about its transitions (a review or a question waiting, a
  // pause nobody asked for, the end), where to, what was sent, and why a
  // subscription could not be used. A live run gets a button to turn it on
  // (through the machine's target) or off; the server checks the target at
  // that moment and refuses one it cannot use. Until the machine's settings
  // are read, nothing is said about them; settings that do not read say so.
  // A live run that notifies about its end gets an outcome filter: the
  // end-of-run notice only for the outcomes the person keeps checked.
  // `actions` renders next to the button (the run page's "Save report").
  import type { Snippet } from 'svelte'
  import { toast } from 'svelte-sonner'
  import { ApiError, fetchNotify, postRunNotify, type NotifyChange } from './api'
  import type { FinishOutcome, MachineNotify, RunNotify } from './api.gen'
  import { NOTIFY_OUTCOMES } from './consts.gen'
  import { NOTIFY_UNREADABLE, OUTCOME_WORDS, canNotify, notifyState, sentLines } from './notify'
  import { isTerminalRunStatus } from './status'
  import { Button } from '$lib/components/ui/button'

  let {
    id,
    workspace = '',
    status,
    notify = null,
    onchange = () => {},
    actions,
  }: {
    id: string
    workspace?: string
    status: string
    notify?: RunNotify | null
    onchange?: () => void
    actions?: Snippet
  } = $props()

  let machine = $state<MachineNotify | null>(null)
  let loaded = $state(false)
  let machineProblem = $state<string | null>(null)
  let busy = $state(false)

  $effect(() => {
    const ws = workspace
    let live = true
    // Another workspace's settings say nothing about this one.
    loaded = false
    machine = null
    machineProblem = null
    fetchNotify(ws)
      .then((m) => {
        if (!live) return
        machine = m
        machineProblem = m.problem ?? null
      })
      .catch(() => {
        if (!live) return
        machine = null
        machineProblem = NOTIFY_UNREADABLE
      })
      .finally(() => {
        if (live) loaded = true
      })
    return () => {
      live = false
    }
  })

  const view = $derived(notifyState(notify))
  const sent = $derived(sentLines(notify))
  const live = $derived(!isTerminalRunStatus(status))
  const canTurnOn = $derived(canNotify(machine))
  const shown = $derived(live || !!notify)
  // The outcome filter, offered while a live run notifies about its end.
  const filtering = $derived(live && view.kind === 'on' && !!notify?.on.includes('finished'))
  const current = $derived<FinishOutcome[]>(notify?.finished ?? [])
  // Keyed by content: a reload with the same filter (every journal write
  // of a live run reloads) never resets a change not applied yet.
  const currentKey = $derived(current.join(','))
  let picked = $state<FinishOutcome[]>([])
  $effect(() => {
    picked = currentKey ? (currentKey.split(',') as FinishOutcome[]) : []
  })
  const filterChanged = $derived(
    picked.length > 0 && (picked.length !== current.length || picked.some((o) => !current.includes(o))),
  )

  function toggle(o: FinishOutcome, checked: boolean) {
    picked = NOTIFY_OUTCOMES.filter((x) => (x === o ? checked : picked.includes(x)))
  }

  /** The toasts of a change: a filter change, or turning notifications on or off. */
  function words(next: NotifyChange): { done: string; failed: string } {
    if (typeof next !== 'boolean') {
      return { done: 'The end-of-run notice filter is changed', failed: 'Could not change the filter' }
    }
    return next
      ? { done: 'Notifications are on for this run', failed: 'Notifications cannot be sent' }
      : { done: 'Notifications are off for this run', failed: 'Could not turn notifications off' }
  }

  async function change(next: NotifyChange) {
    const said = words(next)
    busy = true
    try {
      const res = await postRunNotify(id, next, workspace)
      for (const w of res.warnings ?? []) toast.warning(w)
      toast.success(said.done)
      onchange()
    } catch (e) {
      const detail = e instanceof ApiError && e.detail ? e.detail : String(e)
      toast.error(said.failed, { description: detail })
    } finally {
      busy = false
    }
  }
</script>

<div class="notify-bar" data-testid="run-notify">
  {#if shown}
    <div class="notify-text">
      <span class="notify-word">Notifications</span>
      {#if view.kind === 'on'}
        <span data-testid="run-notify-state">on</span>
        <span class="notify-target" data-testid="run-notify-target">{view.target}</span>
        <span class="muted">about {view.on}{view.source ? `, set by ${view.source}` : ''}</span>
      {:else}
        <span data-testid="run-notify-state">off</span>
        {#if view.problem}
          <span class="notify-problem" data-testid="run-notify-problem">{view.problem}</span>
        {:else if live && machineProblem}
          <span class="notify-problem" data-testid="run-notify-machine-problem">{machineProblem}</span>
        {:else if live && loaded && !canTurnOn}
          <span class="muted" data-testid="run-notify-no-target"
            >no notify target on this machine (apb notify)</span
          >
        {/if}
      {/if}
      {#each sent as line (line)}
        <span class="notify-sent" data-testid="run-notify-sent">{line}</span>
      {/each}
    </div>
    {#if filtering}
      <fieldset class="notify-filter" data-testid="run-notify-filter" disabled={busy}>
        <legend>End-of-run notice when the run</legend>
        {#each NOTIFY_OUTCOMES as o (o)}
          <label class="notify-outcome">
            <input
              type="checkbox"
              data-testid={`run-notify-outcome-${o}`}
              checked={picked.includes(o)}
              onchange={(e) => toggle(o, e.currentTarget.checked)}
            />
            {OUTCOME_WORDS[o]}
          </label>
        {/each}
        <Button
          variant="outline"
          size="sm"
          data-testid="run-notify-filter-apply"
          disabled={busy || !filterChanged}
          onclick={() => change({ finished: picked })}>Apply</Button
        >
      </fieldset>
    {/if}
  {/if}
  <div class="notify-actions">
    {#if live && view.kind === 'on'}
      <Button
        variant="outline"
        size="sm"
        data-testid="run-notify-off"
        disabled={busy}
        onclick={() => change(false)}>Stop notifying</Button
      >
    {:else if live && canTurnOn}
      <Button
        variant="outline"
        size="sm"
        data-testid="run-notify-on"
        disabled={busy}
        onclick={() => change(true)}>Notify me</Button
      >
    {/if}
    {@render actions?.()}
  </div>
</div>

<style>
  /* Plain CSS over the app's variables. */
  .notify-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 16px;
    padding: 6px 16px;
    border-bottom: 1px solid var(--border);
    font-size: 12px;
  }
  .notify-text {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 8px;
    min-width: 0;
  }
  .notify-word {
    font-weight: 600;
  }
  .notify-target {
    font-family: var(--font-mono, ui-monospace, monospace);
    overflow-wrap: anywhere;
  }
  .notify-problem {
    color: var(--destructive);
    overflow-wrap: anywhere;
  }
  .notify-sent {
    color: var(--muted-foreground);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .notify-filter {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 12px;
    min-width: 0;
    margin: 0;
    padding: 0;
    border: 0;
  }
  .notify-filter legend {
    float: left;
    margin-right: 4px;
    padding: 0;
    color: var(--muted-foreground);
  }
  .notify-outcome {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }
  .notify-outcome input {
    accent-color: var(--primary);
  }
  .notify-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-left: auto;
  }
</style>
