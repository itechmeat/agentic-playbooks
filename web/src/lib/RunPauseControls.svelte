<script lang="ts">
  // Operator pause and resume of a run (issue #200). A strip under the run's
  // title: the pause (reason, by, since, until) when there is one, a Pause
  // button for a running run, and a Resume button for a paused run or one
  // whose driver is gone. Pause opens a small form (drain by default, or now,
  // a reason, an optional until and keeping the busy lock). Resume of a run
  // that has no consent recorded for its irreversible effects is refused by
  // the server; the same consent dialog the Run button uses asks, and its
  // confirmation sends the refusal's nonce.
  import { toast } from 'svelte-sonner'
  import { ApiError, pauseRun, resumeRun } from './api'
  import { consentStep } from './irreversibleconsent'
  import IrreversibleRunDialog from './IrreversibleRunDialog.svelte'
  import { canPause, canResume, pauseBody, pauseDetails, pauseWord, type PauseForm } from './pause'
  import type { PauseInfo } from './types'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'

  let {
    id,
    workspace = '',
    status,
    pause = null,
    driverDead = false,
    playbookId = '',
    onchange = () => {},
  }: {
    id: string
    workspace?: string
    status: string
    pause?: PauseInfo | null
    driverDead?: boolean
    playbookId?: string
    onchange?: () => void
  } = $props()

  let formOpen = $state(false)
  let busy = $state(false)
  let form = $state<PauseForm>({ mode: 'drain', reason: '', until: '', keepLock: false })

  let irreversibleOpen = $state(false)
  let irreversibleSources = $state<string[]>([])
  let irreversibleNonce = $state('')

  const showPause = $derived(canPause(status, pause))
  const showResume = $derived(canResume(status, driverDead))
  const word = $derived(pauseWord(pause))
  const details = $derived(pause ? pauseDetails(pause) : [])

  async function submitPause() {
    busy = true
    try {
      const now = form.mode === 'now'
      const res = await pauseRun(id, pauseBody(form), workspace)
      formOpen = false
      form = { mode: 'drain', reason: '', until: '', keepLock: false }
      toast.success(res.outcome === 'paused' ? 'Run paused' : 'Pausing the run', {
        description: now ? 'Running nodes are interrupted.' : 'Running nodes finish first.',
      })
      onchange()
    } catch (e) {
      toast.error('Pause failed', { description: String(e) })
    } finally {
      busy = false
    }
  }

  async function resume(consentNonce?: string) {
    busy = true
    try {
      const res = await resumeRun(id, workspace, consentNonce)
      toast.success(res.resumed_early ? 'Resumed before its until time' : 'Run resumed')
      onchange()
    } catch (e) {
      if (e instanceof ApiError && e.code === 'irreversible_requires_confirmation') {
        const step = consentStep(e.body, consentNonce)
        if (step.kind === 'stop') {
          toast.error('Resume refused', { description: step.message })
        } else {
          irreversibleSources = step.sources
          irreversibleNonce = step.nonce
          irreversibleOpen = true
        }
      } else if (e instanceof ApiError && e.code === 'workdir_busy') {
        // Another run holds the busy lock of this run's tree: nothing was
        // started and the run stays paused.
        toast.error('Cannot resume: the workdir is busy', {
          description: `${e.detail ?? e.message}. The run stays paused; resume it once the other run finishes.`,
        })
      } else {
        toast.error('Resume failed', { description: e instanceof Error ? e.message : String(e) })
      }
    } finally {
      busy = false
    }
  }
</script>

{#if pause || showPause || showResume}
  <div class="pause-bar" data-testid="run-pause-bar">
    {#if pause && word}
      <div class="pause-text" data-testid="run-pause-info">
        <span class="pause-word">{word}</span>
        <span class="pause-reason">{pause.reason}</span>
        <span class="pause-details">{details.join(', ')}</span>
      </div>
    {/if}
    <div class="pause-actions">
      {#if showPause}
        <Button
          variant="outline"
          size="sm"
          data-testid="run-pause-button"
          disabled={busy}
          onclick={() => (formOpen = !formOpen)}
        >
          Pause
        </Button>
      {/if}
      {#if showResume}
        <Button
          variant="outline"
          size="sm"
          data-testid="run-resume-button"
          disabled={busy}
          onclick={() => resume()}
        >
          Resume
        </Button>
      {/if}
    </div>
  </div>
  {#if formOpen && showPause}
    <form
      class="pause-form"
      data-testid="run-pause-form"
      onsubmit={(e) => {
        e.preventDefault()
        void submitPause()
      }}
    >
      <fieldset class="pause-modes">
        <legend>Running nodes</legend>
        <label>
          <input type="radio" name="pause-mode" value="drain" bind:group={form.mode} />
          Let them finish (drain)
        </label>
        <label>
          <input type="radio" name="pause-mode" value="now" bind:group={form.mode} />
          Interrupt them now; they run again on resume
        </label>
      </fieldset>
      <label class="pause-field">
        Reason
        <Input data-testid="run-pause-reason" bind:value={form.reason} placeholder="optional" />
      </label>
      <label class="pause-field">
        Resume automatically
        <Input
          data-testid="run-pause-until"
          bind:value={form.until}
          placeholder="a duration such as 90m or 2h, or an RFC 3339 time (optional)"
        />
      </label>
      <label class="pause-check">
        <input type="checkbox" bind:checked={form.keepLock} />
        Keep the working-tree lock while paused
      </label>
      <div class="pause-form-actions">
        <Button type="submit" size="sm" data-testid="run-pause-submit" disabled={busy}>
          Pause run
        </Button>
        <Button type="button" variant="ghost" size="sm" onclick={() => (formOpen = false)}>
          Cancel
        </Button>
      </div>
    </form>
  {/if}
{/if}

<IrreversibleRunDialog
  bind:open={irreversibleOpen}
  {playbookId}
  sources={irreversibleSources}
  action="resume"
  onconfirm={() => resume(irreversibleNonce)}
/>

<style>
  /* Plain CSS over the palette: the pause line and controls sit inside the
     run rail, so no full-width strip of their own. */
  .pause-bar {
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 12px;
  }
  .pause-text {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 8px;
    min-width: 0;
    padding: 6px 8px;
    border: 1px solid color-mix(in oklab, var(--c-amber) 55%, transparent);
    background: color-mix(in oklab, var(--c-amber) 8%, transparent);
  }
  .pause-word {
    font-weight: 700;
    color: var(--c-amber);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .pause-reason {
    overflow-wrap: anywhere;
  }
  .pause-details {
    color: var(--c-muted);
    font-size: 11px;
  }
  .pause-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .pause-actions:empty {
    display: none;
  }
  .pause-form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 8px;
    padding: 10px 12px;
    border: 1px solid var(--c-line-2);
    background: var(--c-panel-2);
    font-size: 12px;
  }
  .pause-modes {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 0;
    padding: 0;
    border: 0;
  }
  .pause-modes legend {
    padding: 0;
    margin-bottom: 4px;
    color: var(--c-muted);
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .pause-modes label,
  .pause-check {
    display: flex;
    align-items: flex-start;
    gap: 6px;
  }
  .pause-modes input,
  .pause-check input {
    margin-top: 3px;
    accent-color: var(--c-violet);
  }
  .pause-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    color: var(--c-muted);
  }
  .pause-form-actions {
    display: flex;
    gap: 8px;
  }
</style>
