<script lang="ts">
  // A project's name in a picker; a git linked worktree is indented under
  // the project it belongs to.
  let { name, depth = 0 }: { name: string; depth?: number } = $props()
</script>

{#if depth > 0}
  <span class="worktree" title="git worktree">{name}</span>
{:else}
  {name}
{/if}

<style>
  .worktree {
    padding-inline-start: 1rem;
  }
</style>
