<script lang="ts">
  // The publish step of a run: per publish node, the exact plan a person
  // consents to (the URL, the branch, what the remote holds now, every
  // commit with its tags), the buttons that post the decision with the
  // plan's nonce, and after the push its ledger state, with the resolve
  // command when nobody knows whether it arrived. Every text from the run
  // (subjects, URLs, results) renders as plain text. A dashboard without
  // keys takes no publish consent (anything on the machine could post one),
  // so there the panel offers the terminal command instead of the button.
  import type { PublishView } from './api.gen'
  import { authState } from './auth.svelte'
  import * as Card from '$lib/components/ui/card'
  import { Button } from '$lib/components/ui/button'
  import {
    approveCommand,
    commitCount,
    commitTags,
    needsALook,
    publishCards,
    pushLine,
    refusalText,
    remoteState,
    resolveCommand,
    shortSha,
    type PublishGate,
  } from './publish'

  let {
    runId,
    views = [],
    gates = [],
    posting = null,
    onDecide,
  }: {
    runId: string
    views?: PublishView[] | null
    gates?: PublishGate[]
    /** `node:decision` of the decision in flight, or null. */
    posting?: string | null
    onDecide: (node: string, decision: 'publish' | 'skip', nonce: string) => void | Promise<void>
  } = $props()

  const cards = $derived(publishCards(views, gates))
  const authSnapshot = authState()
  // Keyless only once the status read answered: until then the button
  // stays (the server refuses a keyless consent anyway).
  const keyless = $derived(authSnapshot.checked && !authSnapshot.required)
</script>

{#if cards.length}
  <Card.Root class="publish-root" data-testid="publish-panel">
    <Card.Header>
      <Card.Title>Publish</Card.Title>
    </Card.Header>
    <Card.Content>
      <div class="cards">
        {#each cards as c (c.node)}
          <section class="card" class:waiting={c.awaiting_consent} data-testid="publish-card">
            <div class="head">
              <span class="mono">{c.node}</span>
              {#if c.remote}<span class="muted">remote {c.remote}</span>{/if}
            </div>
            {#if c.pin_refusal}
              <p class="bad" data-testid="publish-pin-refusal">
                Target not pinned: {refusalText(c.pin_refusal)} <span class="code">{c.pin_refusal}</span>
              </p>
            {/if}
            {#if c.awaiting_consent && c.prompt}
              <pre class="guidance" data-testid="publish-guidance">{c.prompt}</pre>
            {/if}
            {#if c.plan}
              {@const plan = c.plan}
              <dl class="plan" data-testid="publish-plan">
                <dt>Push</dt>
                <dd class="mono" data-testid="publish-after">{plan.after}</dd>
                <dt>To</dt>
                <dd><span class="mono" data-testid="publish-url">{plan.url}</span></dd>
                <dt>Branch</dt>
                <dd class="mono" data-testid="publish-branch">{plan.branch}</dd>
                <dt>Remote now</dt>
                <dd data-testid="publish-remote">{remoteState(plan)}</dd>
                <dt>Commits</dt>
                <dd>{commitCount(plan)}</dd>
              </dl>
              <ul class="commits" data-testid="publish-commits">
                {#each plan.commits as commit (commit.sha)}
                  <li>
                    <span class="mono">{shortSha(commit.sha)}</span>
                    <span class="subject">{commit.subject}</span>
                    {#each commitTags(commit) as tag, i (i)}
                      <span class="tag" class:old={!commit.this_run}>{tag}</span>{' '}
                    {/each}
                  </li>
                {/each}
                {#if plan.omitted > 0}
                  <li class="muted">and {plan.omitted} more</li>
                {/if}
              </ul>
              {#if plan.outside_push_suspected}
                <p class="bad" data-testid="publish-outside">
                  Commits of this run are already on the remote: something pushed them outside the publish step.
                </p>
              {/if}
              {#if c.awaiting_consent}
                <p class="muted">Consent covers exactly this plan; a changed plan asks again.</p>
                {#if keyless}
                  <div class="look" data-testid="publish-keyless">
                    <p>
                      This dashboard has no keys, so it takes no publish consent: anything on this machine could post
                      one. Approve from a terminal, or issue a key with <code class="mono">apb server key issue</code> and
                      sign in:
                    </p>
                    <code class="mono">{approveCommand(runId, c.node, plan.nonce)}</code>
                  </div>
                {/if}
                <div class="actions">
                  {#if !keyless}
                    <Button
                      size="sm"
                      data-testid="publish-approve"
                      disabled={posting !== null}
                      onclick={() => onDecide(c.node, 'publish', plan.nonce)}
                    >
                      Publish exactly this
                    </Button>
                  {/if}
                  <Button
                    variant="outline"
                    size="sm"
                    data-testid="publish-skip"
                    disabled={posting !== null}
                    onclick={() => onDecide(c.node, 'skip', plan.nonce)}
                  >
                    Skip
                  </Button>
                </div>
              {/if}
            {/if}
            {#if c.push}
              <p class:bad={needsALook(c)} data-testid="publish-push">{pushLine(c.push)}</p>
              {#if needsALook(c)}
                <div class="look" data-testid="publish-needs-look">
                  <p>Needs a look: nobody knows whether the push arrived, and apb never repeats it on its own. Check the remote, then record what you found:</p>
                  <code class="mono">{resolveCommand(runId, c.push)}</code>
                </div>
              {/if}
            {/if}
            {#if c.result}
              <p class="muted" data-testid="publish-result">Result: <span class="mono">{c.result}</span></p>
            {/if}
          </section>
        {/each}
      </div>
    </Card.Content>
  </Card.Root>
{/if}

<style>
  /* Plain CSS over the app's variables. The card keeps its full height in
     the run page's scrolling side column. */
  :global(.publish-root) {
    flex-shrink: 0;
  }
  .guidance {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: inherit;
    font-size: 12px;
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: 12px;
    font-size: 12px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .card.waiting {
    border-left: 2px solid var(--primary);
    padding-left: 8px;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 8px;
  }
  .mono,
  .code {
    font-family: var(--font-mono, ui-monospace, monospace);
    overflow-wrap: anywhere;
  }
  .code {
    color: var(--muted-foreground);
    font-size: 11px;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .bad {
    color: var(--destructive);
  }
  p {
    margin: 0;
  }
  .plan {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 2px 8px;
    margin: 0;
  }
  .plan dt {
    color: var(--muted-foreground);
  }
  .plan dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .commits {
    margin: 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .commits li {
    overflow-wrap: anywhere;
  }
  .subject {
    margin: 0 4px;
  }
  .tag {
    display: inline-block;
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0 6px;
    margin-right: 2px;
    font-size: 11px;
  }
  .tag.old {
    border-color: var(--destructive);
    color: var(--destructive);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .look {
    display: flex;
    flex-direction: column;
    gap: 4px;
    border: 1px solid var(--destructive);
    border-radius: 6px;
    padding: 6px 8px;
  }
  .look code {
    user-select: all;
  }
</style>
