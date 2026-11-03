<script lang="ts">
  import { Button } from '$lib/components/ui/button'
  import { authState } from './auth.svelte'
  import { scrollToSnapshot } from './results'
  import { refusalKey, reviewCommand, type ReviewEntry } from './rungates'
  import type { BoundSnapshot } from './api.gen'
  import UnattendedNote from './UnattendedNote.svelte'

  // The run page's pending human reviews: per gate its guidance, the
  // decision model's recommendation, what an unattended run does there, and
  // one button per option. The gate's `decide_review` next action is drawn
  // here, never in the next-actions panel.
  //
  // A gate with `approves:` shows the snapshots its request is bound to,
  // each linked to its row in the results panel: an approval is a fact
  // about exactly those stored bytes. A person-only gate takes a decision
  // only from a person's channel; a dashboard without keys is not one
  // (anything on this machine reaches it), so there the card offers the
  // terminal command instead of its buttons. `refusals` holds the server's
  // sentence for a decision it refused, keyed by `refusalKey`.
  let {
    reviews,
    posting = null,
    refusals = {},
    onDecide,
  }: {
    reviews: ReviewEntry[]
    /** `<node>:<option>` of the decision in flight. */
    posting?: string | null
    refusals?: Record<string, string>
    onDecide: (review: ReviewEntry, option: string) => void | Promise<void>
  } = $props()

  const authSnapshot = authState()
  // Keyless only once the status read answered: until then the buttons
  // stay (the server refuses a keyless person-only decision anyway).
  const keyless = $derived(authSnapshot.checked && !authSnapshot.required)

  const reasonWord = { not_run: 'not run', no_snapshot: 'no snapshot' } as const

  function snapshotHead(s: BoundSnapshot): string {
    return [s.node, `execution ${s.execution}`, ...(s.item ? [`item ${s.item}`] : [])].join(', ')
  }
  function snapshotFlags(s: BoundSnapshot): string[] {
    return [
      ...(s.incomplete ? ['incomplete'] : []),
      ...(s.redacted ? ['redacted'] : []),
      ...(s.omitted ? [`${s.omitted} omitted`] : []),
    ]
  }
</script>

{#if reviews.length}
  <section class="box glow reviews" style:--k="var(--c-amber)" data-testid="review-panel">
    <span class="box-cap">Human review</span>
    {#each reviews as pr (pr.node)}
      {@const cli = pr.personOnly && keyless}
      <div class="gate">
        <span class="node">{pr.node}</span>
        {#if pr.prompt}
          <pre class="prose-text guidance">{pr.prompt}</pre>
        {/if}
        {#if pr.recommendation}
          <p class="recommendation" data-testid="review-recommendation">{pr.recommendation}</p>
        {/if}
        {#if pr.unattended}
          <UnattendedNote outcome={pr.unattended} testid="review-unattended" />
        {/if}
        {#if pr.binding}
          <div class="binding" data-testid="review-binding">
            <p class="note">Approving approves exactly these stored snapshots:</p>
            <ul>
              {#each pr.binding.snapshots as s (s.snapshot_id)}
                <li data-testid="review-binding-snapshot">
                  <span>{snapshotHead(s)}</span>
                  <span class="mono" title={s.snapshot_id}>{s.snapshot_id.slice(0, 12)}</span>
                  {#each snapshotFlags(s) as f (f)}<span class="flag">{f}</span>{' '}{/each}
                  <button
                    type="button"
                    class="link"
                    data-testid="review-binding-link"
                    onclick={() => scrollToSnapshot(s.snapshot_id)}
                  >
                    Show in results
                  </button>
                </li>
              {/each}
              {#each pr.binding.missing ?? [] as m (m.node)}
                <li class="missing" data-testid="review-binding-missing">
                  {m.node}: {reasonWord[m.reason] ?? m.reason}; approving does not approve it
                </li>
              {/each}
            </ul>
            <p class="note mono" data-testid="review-binding-nonce">binding {pr.binding.nonce}</p>
          </div>
        {/if}
        {#if pr.personOnly && !cli}
          <p class="note" data-testid="review-person-only">
            Only a person decides this gate: at a terminal, or here while signed in.
          </p>
        {/if}
        {#if cli}
          <div class="cli" data-testid="review-cli">
            <p>
              Only a person decides this gate, and this dashboard has no keys, so anything on this machine could post
              here. Decide from a terminal, or issue a key with <code class="mono">apb server key issue</code> and sign
              in:
            </p>
            {#each pr.options as opt (opt)}
              <code class="mono" data-testid="review-cli-command">{reviewCommand(pr, opt)}</code>
            {/each}
          </div>
        {:else}
          <div class="options">
            {#each pr.options as opt (opt)}
              <Button
                variant="outline"
                size="sm"
                data-testid="review-option"
                onclick={() => onDecide(pr, opt)}
                disabled={posting === `${pr.node}:${opt}`}
              >
                {opt}
              </Button>
            {/each}
          </div>
        {/if}
        {#if refusals[refusalKey(pr)]}
          <p class="refusal" role="alert" data-testid="review-refusal">{refusals[refusalKey(pr)]}</p>
        {/if}
      </div>
    {/each}
  </section>
{/if}

<style>
  .reviews {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 16px 12px;
  }
  .gate {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .node {
    color: var(--c-amber);
    font-size: 12px;
  }
  .guidance {
    margin: 0;
    font-size: 14px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .recommendation,
  .note {
    margin: 0;
    color: var(--muted-foreground);
    font-size: 12px;
  }
  .binding ul {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 2px 0;
    padding: 0;
    list-style: none;
    font-size: 12px;
  }
  .binding li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 6px;
    min-width: 0;
  }
  .binding .missing {
    color: var(--c-red);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
    overflow-wrap: anywhere;
  }
  .flag {
    padding: 0 4px;
    border: 1px solid var(--c-amber);
    border-radius: 3px;
    color: var(--c-amber);
    font-size: 10px;
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--primary);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  .cli {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .cli p {
    margin: 0;
  }
  .cli code {
    display: block;
    padding: 4px 6px;
    border-radius: 4px;
    background: var(--muted);
    font-size: 11px;
  }
  .refusal {
    margin: 0;
    color: var(--c-red);
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .options {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .options :global(button) {
    height: 28px;
  }
</style>
