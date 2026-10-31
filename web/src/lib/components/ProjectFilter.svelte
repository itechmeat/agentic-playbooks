<script lang="ts">
  import * as Select from '$lib/components/ui/select'
  import { cn } from '$lib/utils'
  import {
    ALL_PROJECTS,
    projectFilter,
    projectOptions,
  } from '$lib/projectfilter'

  type Item = { workspace_id: string; project?: string | null }

  let { items = [] as Item[], class: className = 'mb-4' }: { items?: Item[]; class?: string } =
    $props()

  const options = $derived(projectOptions(items))
  const knownIds = $derived(new Set(options.map((o) => o.id)))

  // Unknown stored id: show "All projects" without mutating the store.
  const displayValue = $derived(
    $projectFilter === ALL_PROJECTS || knownIds.has($projectFilter)
      ? $projectFilter
      : ALL_PROJECTS,
  )

  const triggerLabel = $derived(
    displayValue === ALL_PROJECTS
      ? 'All projects'
      : (options.find((o) => o.id === displayValue)?.label ?? 'All projects'),
  )

  function onValueChange(v: string | undefined) {
    if (v == null) return
    projectFilter.set(v)
  }
</script>

<div class={cn('flex items-center gap-2', className)}>
  <Select.Root type="single" value={displayValue} {onValueChange}>
    <Select.Trigger size="sm" class="w-48" aria-label="Project filter">
      {triggerLabel}
    </Select.Trigger>
    <Select.Content>
      <Select.Item value={ALL_PROJECTS} label="All projects">All projects</Select.Item>
      {#each options as o (o.id)}
        <Select.Item value={o.id} label={o.label}>{o.label}</Select.Item>
      {/each}
    </Select.Content>
  </Select.Root>
</div>
