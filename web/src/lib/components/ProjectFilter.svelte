<script lang="ts">
  import * as Select from '$lib/components/ui/select'
  import { cn } from '$lib/utils'
  import ProjectLabel from '$lib/components/ProjectLabel.svelte'
  import { fetchProjects } from '$lib/api/core'
  import {
    ALL_PROJECTS,
    nestProjectOptions,
    projectFilter,
    projectOptions,
  } from '$lib/projectfilter'
  import type { Project } from '$lib/types'

  type Item = { workspace_id: string; project?: string | null }

  let { items = [] as Item[], class: className = 'mb-4' }: { items?: Item[]; class?: string } =
    $props()

  // The registry read only orders the options (worktrees under their parent);
  // when it fails the picker falls back to the flat list.
  let projects = $state<Project[]>([])
  $effect(() => {
    fetchProjects()
      .then((p) => (projects = p))
      .catch(() => {})
  })

  const options = $derived(projectOptions(items))
  const nested = $derived(nestProjectOptions(options, projects))
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
      {#each nested as { row: o, depth } (o.id)}
        <Select.Item value={o.id} label={o.label}>
          <ProjectLabel name={o.label} {depth} />
        </Select.Item>
      {/each}
    </Select.Content>
  </Select.Root>
</div>
