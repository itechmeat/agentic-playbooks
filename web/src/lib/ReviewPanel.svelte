<script lang="ts">
  import { Button } from '$lib/components/ui/button'
  import type { ReviewEntry } from './rungates'
  import UnattendedNote from './UnattendedNote.svelte'

  // The run page's pending human reviews: per gate its guidance, the
  // decision model's recommendation, what an unattended run does there, and
  // one button per option. The gate's `decide_review` next action is drawn
  // here, never in the next-actions panel.
  let {
    reviews,
    posting = null,
    onDecide,
  }: {
    reviews: ReviewEntry[]
    /** `<node>:<option>` of the decision in flight. */
    posting?: string | null
    onDecide: (review: ReviewEntry, option: string) => void | Promise<void>
  } = $props()
</script>

{#if reviews.length}
  <section class="box glow reviews" style:--k="var(--c-amber)" data-testid="review-panel">
    <span class="box-cap">Human review</span>
    {#each reviews as pr (pr.node)}
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
        <div class="options">
          {#each pr.options as opt (opt)}
            <Button
              variant="outline"
              size="sm"
              onclick={() => onDecide(pr, opt)}
              disabled={posting === `${pr.node}:${opt}`}
            >
              {opt}
            </Button>
          {/each}
        </div>
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
  .recommendation {
    margin: 0;
    color: var(--muted-foreground);
    font-size: 12px;
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
