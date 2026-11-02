<script lang="ts">
  // What the run needs next, read only from the server's `next_actions`:
  // the actions of the run and of its live sub-playbook children, the
  // primary one marked. An action the page can take (an answer, a review or
  // publish decision, a decision on an unknown send, a resume, starting a
  // waiting retry now) gets its
  // buttons; every other one gets its `apb` command line to copy. Each post
  // goes to the action's own run (a child's request reaches the child) and
  // names the request it answers, through `runposts`, the same functions the
  // question panel and the review and publish cards use. A decision on an
  // unknown send stays possible after the run ended, so the panel shows on
  // finished runs too. Every text from the run renders as plain text.
  // Each wait has its controls once on the page: an action another control
  // already takes (the question panel, the human review card, the publish
  // panel, the resume button; named by `controls`) is left out here.
  import { toast } from 'svelte-sonner'
  import { ApiError, resumeRun } from './api'
  import type { NextAction, RunActions } from './api.gen'
  import { authState } from './auth.svelte'
  import { launchQuestion, launchStep } from './launch'
  import { consentStep } from './irreversibleconsent'
  import IrreversibleRunDialog from './IrreversibleRunDialog.svelte'
  import {
    autoLine,
    choiceArg,
    cliLine,
    givenArg,
    intentConsequence,
    isExecutable,
    orderedActions,
    personTakes,
    shownElsewhere,
    suppliedArg,
  } from './nextactions'
  import { actionTarget, postDecision, postIntentDecision, postReply } from './runposts'
  import * as AlertDialog from '$lib/components/ui/alert-dialog'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'

  let {
    actions = null,
    runId,
    playbook = '',
    workspace = '',
    controls = [],
    onchange = () => {},
  }: {
    actions?: RunActions | null
    /** The run the page shows; an action of another run is a child's. */
    runId: string
    playbook?: string
    workspace?: string
    /** The controls the page shows elsewhere (`pageControls`). */
    controls?: readonly string[]
    onchange?: () => void | Promise<void>
  } = $props()

  const items = $derived(orderedActions(actions).filter(({ action }) => !shownElsewhere(action, runId, controls)))
  const authSnapshot = authState()
  // A dashboard without keys takes no publish consent (the publish panel's
  // rule) and no decision on a send: those give way to the command line.
  const keyless = $derived(authSnapshot.checked && !authSnapshot.required)

  // An action's identity across reloads: the list reorders and shrinks as
  // waits settle, so a draft or a busy mark follows the action, never its
  // position.
  const keyOf = (a: NextAction) =>
    `${a.run_id}:${a.code}:${a.target.node ?? ''}:${a.target.task_id ?? ''}:${a.target.request?.seq ?? ''}:${givenArg(a, 'intent') ?? ''}`

  // The action in flight, by its key; one at a time.
  let busy = $state<string | null>(null)
  let drafts = $state<Record<string, string>>({})

  // The decision on an unknown send the dialog asks to confirm. It stays
  // while the dialog closes, so the closing dialog keeps its wording.
  let confirmOpen = $state(false)
  let pendingIntent = $state<{ key: string; action: NextAction; sent: boolean } | null>(null)

  let irreversibleOpen = $state(false)
  let irreversibleSources = $state<string[]>([])
  let irreversibleNonce = $state('')
  let resumeTarget = $state<string | null>(null)

  const isChild = (a: NextAction) => a.run_id !== runId

  // Reloads the page's data; a reload that fails after a post that went
  // through is not the post's failure.
  const reload = () => Promise.resolve(onchange()).catch(() => {})

  // Runs one post; whether it went through.
  async function act(key: string, work: () => Promise<unknown>, done: string): Promise<boolean> {
    busy = key
    let ok = false
    try {
      await work()
      ok = true
      toast.success(done)
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        // The request was replaced or already settled: show what holds now.
        toast.error('This request changed', { description: e.detail ?? e.message })
      } else {
        toast.error('The action failed', { description: e instanceof Error ? e.message : String(e) })
      }
    } finally {
      busy = null
    }
    await reload()
    return ok
  }

  function answer(a: NextAction, value: string) {
    void act(keyOf(a), () => postReply(actionTarget(a), value, workspace), 'Answer posted')
  }

  async function submitDraft(e: SubmitEvent, a: NextAction) {
    e.preventDefault()
    const key = keyOf(a)
    const value = (drafts[key] ?? '').trim()
    if (!value) return
    // A failed post keeps what the person typed.
    if (await act(key, () => postReply(actionTarget(a), value, workspace), 'Answer posted')) {
      drafts[key] = ''
    }
  }

  function decide(a: NextAction, decision: string) {
    void act(keyOf(a), () => postDecision(actionTarget(a), decision, workspace), 'Decision posted')
  }

  function askIntent(action: NextAction, sent: boolean) {
    pendingIntent = { key: keyOf(action), action, sent }
    confirmOpen = true
  }

  function confirmIntent() {
    const p = pendingIntent
    confirmOpen = false
    if (!p) return
    const intent = givenArg(p.action, 'intent')
    if (!intent) {
      toast.error('This send has no intent id; use the command line instead')
      return
    }
    void act(
      p.key,
      () => postIntentDecision(p.action.run_id, intent, p.sent, workspace),
      p.sent ? 'Recorded: the send went out' : 'Recorded: the send did not go out',
    )
  }

  // The launch nonce and the waiting retry the last resume carried, for
  // its consent retry.
  let resumeLaunchNonce: string | undefined
  let resumeRetry: { node?: string } | undefined

  async function resume(
    key: string,
    target: string,
    consentNonce?: string,
    launchNonce?: string,
    retryNow?: { node?: string },
  ) {
    busy = key
    resumeLaunchNonce = launchNonce
    resumeRetry = retryNow
    try {
      // A waiting retry started now: a live run only starts the retry, a
      // released or paused one is resumed with it due.
      const out = await resumeRun(target, workspace, { consentNonce, launchNonce, ...(retryNow ? { retryNow } : {}) })
      toast.success(retryNow ? (out?.live ? 'Retry started' : 'Run resumed with the retry due') : 'Run resumed')
      void reload()
    } catch (e) {
      // A never-authorized run of an `ask` playbook on a dashboard without
      // keys: resumed only with its refusal's nonce, once confirmed here.
      const sent = launchNonce ? { nonce: launchNonce } : undefined
      const launchNext = e instanceof ApiError ? launchStep(e.body, sent) : null
      if (launchNext?.kind === 'confirm') {
        if (window.confirm(launchQuestion(launchNext))) {
          const nonce = launchNext.confirmation.nonce
          queueMicrotask(() => void resume(key, target, consentNonce, nonce, retryNow))
        }
      } else if (launchNext) {
        toast.error('Resume refused', { description: launchNext.message })
      } else if (e instanceof ApiError && e.code === 'irreversible_requires_confirmation') {
        const step = consentStep(e.body, consentNonce)
        if (step.kind === 'stop') {
          toast.error('Resume refused', { description: step.message })
        } else {
          irreversibleSources = step.sources
          irreversibleNonce = step.nonce
          resumeTarget = target
          irreversibleOpen = true
        }
      } else {
        toast.error('Resume failed', { description: e instanceof Error ? e.message : String(e) })
      }
    } finally {
      busy = null
    }
  }

  async function copy(line: string) {
    try {
      await navigator.clipboard.writeText(line)
      toast.success('Command copied')
    } catch {
      toast.error('Could not copy; select the line instead')
    }
  }

  // A publish consent is given on this page only for a plan it shows: its
  // own run's, in the publish panel, and only with keys. A sub-playbook
  // run's publish gate offers skip here, and its command line.
  const noPublishHere = (a: NextAction) => a.code === 'decide_publish' && (keyless || isChild(a))

  // The options a decision shows: the publish option only where the page
  // may take a publish consent.
  function decisionOptions(a: NextAction): string[] {
    const options = choiceArg(a, 'decision')?.options ?? []
    return noPublishHere(a) ? options.filter((o) => o !== 'publish') : options
  }

  // The page takes it: a code it has buttons for, from a person. A
  // decision on an unknown send needs a dashboard with keys (the server
  // refuses it otherwise): a keyless page shows its command line.
  const takes = (a: NextAction) =>
    isExecutable(a) && personTakes(a) && !(a.code === 'resolve_intent' && keyless)
  // The command line shows for what the page does not take, and next to a
  // publish decision the page cannot consent to.
  const showsLine = (a: NextAction) => !takes(a) || noPublishHere(a)
</script>

{#snippet commandLine(a: NextAction)}
  {@const line = cliLine(a)}
  {#if line}
    <div class="cli" data-testid="next-action-cli">
      <code class="line">{line}</code>
      <Button variant="ghost" size="xs" onclick={() => copy(line)}>Copy</Button>
    </div>
  {:else if a.code.startsWith('supervisor_')}
    <p class="muted" data-testid="next-action-supervisor">The run's supervisor takes this.</p>
  {:else}
    <p class="muted" data-testid="next-action-no-line">No command line came with this action.</p>
  {/if}
{/snippet}

{#if items.length}
  <section
    class="box next-root"
    class:glow={actions?.stop === 'needs_input'}
    style:--k={actions?.stop === 'needs_input' ? 'var(--c-amber)' : undefined}
    data-testid="next-actions"
  >
    <span class="box-cap">Next actions</span>
    {#if actions?.children_truncated}
      <p class="muted">A sub-playbook run sat too deep to be observed; its waits are not listed here.</p>
    {/if}
    <ol class="list">
      {#each items as { action: a, primary } (keyOf(a))}
        {@const key = keyOf(a)}
        <li class="action" class:primary data-testid="next-action" data-code={a.code}>
          <div class="head">
            {#if primary}<span class="tag lead" data-testid="next-action-primary">primary</span>{/if}
            {#if a.target.node}<span class="node">{a.target.node}</span>{/if}
            {#if isChild(a)}
              <span class="tag child" data-testid="next-action-child">acts on sub-playbook run {a.run_id}</span>
            {/if}
          </div>
          <p class="reason">{a.reason}</p>
          {#if a.auto}
            <p class="muted" data-testid="next-action-auto">{autoLine(a.auto)}</p>
          {/if}
          {#if !a.ready && a.not_ready}
            <p class="bad" data-testid="next-action-not-ready">Not ready: {a.not_ready.detail}</p>
          {/if}

          {#if takes(a)}
            {#if a.code === 'answer_question'}
              {@const choice = choiceArg(a, 'answer')}
              {@const hint = suppliedArg(a, 'answer')}
              {#if choice}
                <div class="buttons">
                  {#each choice.options as opt (opt)}
                    <Button
                      variant={opt === choice.default ? 'default' : 'outline'}
                      size="sm"
                      data-testid="next-action-option"
                      disabled={!a.ready || busy !== null}
                      onclick={() => answer(a, opt)}
                    >
                      {opt}
                    </Button>
                  {/each}
                </div>
              {:else}
                <form class="answer" onsubmit={(e) => submitDraft(e, a)}>
                  <Input
                    type="text"
                    placeholder={hint ?? 'Type an answer'}
                    aria-label="Answer"
                    bind:value={drafts[key]}
                    disabled={!a.ready || busy !== null}
                  />
                  <Button
                    type="submit"
                    size="sm"
                    data-testid="next-action-send"
                    disabled={!a.ready || busy !== null || !(drafts[key] ?? '').trim()}
                  >
                    Send
                  </Button>
                </form>
              {/if}
            {:else if a.code === 'decide_review' || a.code === 'decide_publish'}
              <div class="buttons">
                {#each decisionOptions(a) as opt (opt)}
                  <Button
                    variant="outline"
                    size="sm"
                    data-testid="next-action-option"
                    disabled={!a.ready || busy !== null}
                    onclick={() => decide(a, opt)}
                  >
                    {opt === 'publish' ? 'Publish exactly this' : opt}
                  </Button>
                {/each}
              </div>
            {:else if a.code === 'resolve_intent'}
              {@const noIntent = !givenArg(a, 'intent')}
              <div class="buttons">
                <Button
                  variant="outline"
                  size="sm"
                  data-testid="intent-sent"
                  disabled={!a.ready || busy !== null || noIntent}
                  onclick={() => askIntent(a, true)}
                >
                  It went out
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  data-testid="intent-not-sent"
                  disabled={!a.ready || busy !== null || noIntent}
                  onclick={() => askIntent(a, false)}
                >
                  It did not go out
                </Button>
              </div>
            {:else if a.code === 'resume_run'}
              <div class="buttons">
                <Button
                  size="sm"
                  data-testid="next-action-resume"
                  disabled={!a.ready || busy !== null}
                  onclick={() => resume(key, a.run_id)}
                >
                  Resume
                </Button>
              </div>
            {:else if a.code === 'retry_now'}
              <div class="buttons">
                <Button
                  size="sm"
                  variant="outline"
                  data-testid="next-action-retry-now"
                  disabled={!a.ready || busy !== null}
                  onclick={() => resume(key, a.run_id, undefined, undefined, { node: givenArg(a, 'node') ?? a.target.node })}
                >
                  Retry now
                </Button>
              </div>
            {/if}
          {/if}
          {#if showsLine(a)}
            {@render commandLine(a)}
          {/if}
        </li>
      {/each}
    </ol>
  </section>
{/if}

<AlertDialog.Root bind:open={confirmOpen}>
  <AlertDialog.Content data-testid="intent-dialog">
    <AlertDialog.Header>
      <AlertDialog.Title>
        {pendingIntent?.sent ? 'Record that the send went out?' : 'Record that the send did not go out?'}
      </AlertDialog.Title>
      <AlertDialog.Description>
        Intent <code>{pendingIntent ? givenArg(pendingIntent.action, 'intent') : ''}</code> of run
        <code>{pendingIntent?.action.run_id}</code>. Check the service first. After this,
        <span data-testid="intent-consequence">{intentConsequence(pendingIntent?.sent ?? false)}</span>.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel data-testid="intent-cancel">Cancel</AlertDialog.Cancel>
      <AlertDialog.Action data-testid="intent-confirm" onclick={confirmIntent}>Record it</AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>

<IrreversibleRunDialog
  bind:open={irreversibleOpen}
  playbookId={playbook}
  sources={irreversibleSources}
  action="resume"
  subRun={resumeTarget && resumeTarget !== runId ? resumeTarget : null}
  onconfirm={() => {
    if (resumeTarget)
      void resume(`resume:${resumeTarget}`, resumeTarget, irreversibleNonce, resumeLaunchNonce, resumeRetry)
  }}
/>

<style>
  /* Plain CSS over the app's variables, in the terminal look of the other
     side-column boxes. */
  .next-root {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 16px 16px 12px;
    flex-shrink: 0;
    font-size: 12px;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .action {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .action.primary {
    border-left: 2px solid var(--c-amber);
    padding-left: 8px;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
  }
  .node {
    color: var(--c-amber);
    overflow-wrap: anywhere;
  }
  .lead {
    color: var(--c-amber);
  }
  .child {
    color: var(--c-teal);
    white-space: normal;
    overflow-wrap: anywhere;
  }
  p {
    margin: 0;
  }
  .reason {
    color: var(--foreground);
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .bad {
    color: var(--destructive);
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .answer {
    display: flex;
    gap: 8px;
  }
  .cli {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    border: 1px solid var(--c-line);
    background: var(--c-field);
    padding: 4px 6px;
  }
  .line {
    flex: 1;
    min-width: 0;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    overflow-wrap: anywhere;
    user-select: all;
  }
</style>
