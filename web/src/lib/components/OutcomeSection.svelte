<script lang="ts">
  import type { Snippet } from 'svelte'

  // One titled block of the run page's outcome panels (verdict, goal,
  // commits): a bordered card over the theme variables, no utility classes.
  let { title, testid, children }: { title: string; testid?: string; children: Snippet } = $props()
</script>

<section class="outcome-section" data-testid={testid}>
  <h3 class="outcome-section-title">{title}</h3>
  {@render children()}
</section>

<style>
  /* The design's titled box: a caption cut into the top border. */
  .outcome-section {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 16px 16px 12px;
    border: 1px solid var(--c-line-2);
    background: var(--c-panel);
    color: var(--foreground);
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }
  .outcome-section-title {
    position: absolute;
    top: -0.6em;
    left: 12px;
    max-width: calc(100% - 24px);
    margin: 0;
    padding: 0 8px;
    overflow: hidden;
    background: var(--cap-bg, var(--c-bg));
    color: var(--c-muted);
    font-size: 11px;
    font-weight: 400;
    line-height: 1.1;
    letter-spacing: 0.04em;
    text-transform: lowercase;
    white-space: nowrap;
  }
</style>
