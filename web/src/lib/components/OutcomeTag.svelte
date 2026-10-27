<script lang="ts">
  import type { Snippet } from 'svelte'
  import type { Tone } from '../runverdict'

  // A small status tag of the outcome panels: pass, fail, gap (nobody
  // checked it) or neutral, coloured from the theme variables.
  let { tone = 'neutral', children }: { tone?: Tone; children: Snippet } = $props()
</script>

<span class="outcome-tag {tone}">{@render children()}</span>

<style>
  .outcome-tag {
    display: inline-block;
    flex-shrink: 0;
    padding: 0 0.4rem;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 0.6875rem;
    line-height: 1.1rem;
    white-space: nowrap;
  }
  .pass {
    border-color: var(--success);
    color: var(--success);
  }
  .fail {
    border-color: var(--destructive);
    color: var(--destructive);
  }
  .gap {
    border-color: var(--warning);
    background: color-mix(in oklch, var(--warning) 15%, transparent);
  }
  .neutral {
    color: var(--muted-foreground);
  }
</style>
