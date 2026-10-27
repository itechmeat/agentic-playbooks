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
  .outcome-section {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: var(--radius, 0.5rem);
    background: var(--card);
    color: var(--card-foreground);
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }
  .outcome-section-title {
    font-size: 0.875rem;
    font-weight: 600;
  }
</style>
