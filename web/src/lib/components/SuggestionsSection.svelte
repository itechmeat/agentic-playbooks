<script lang="ts">
  import { deleteSuggestion, fetchProjects, fetchSuggestions } from '../api'
  import {
    sortSuggestions,
    mergeSuggestionRows,
    kindLabel,
    removeMessage,
    suggestionWarnings,
    untilLabel,
    GLOBAL_SOURCE_ID,
    type SuggestionRow,
    type WorkspaceSuggestions,
  } from '../suggestions'
  import { Badge } from '$lib/components/ui/badge'
  import { Button } from '$lib/components/ui/button'
  import * as Card from '$lib/components/ui/card'
  import { toast } from 'svelte-sonner'
  import BellOff from '@lucide/svelte/icons/bell-off'
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert'
  import Trash2 from '@lucide/svelte/icons/trash-2'

  let rows = $state<SuggestionRow[]>([])
  let warnings = $state<string[]>([])
  let loaded = $state(false)
  let removing = $state<string | null>(null)
  // Reactive, and refreshed on every load: a module-level `const now` was
  // captured once at init, so a dashboard left open overnight kept rendering
  // "in 2 days" for a snooze that had already ended.
  let now = $state(Date.now())

  const key = (r: SuggestionRow) => `${r.workspace_id}/${r.scope}/${r.pattern}`

  // One source: whatever the server answered, or the reason it did not. A
  // failure is kept as a warning rather than collapsed into an empty list,
  // which used to make a corrupt store indistinguishable from "nothing
  // silenced here".
  async function loadSource(workspace_id: string, project: string): Promise<WorkspaceSuggestions> {
    try {
      const res = await fetchSuggestions(workspace_id)
      return {
        workspace_id,
        project,
        records: res.suggestions ?? [],
        diagnostics: res.diagnostics ?? [],
      }
    } catch (e) {
      return { workspace_id, project, records: [], error: String(e) }
    }
  }

  async function load() {
    try {
      const projects = await fetchProjects()
      // The workspace-less source first: it is the only one that reports the
      // global store on a machine with zero registered projects, and listing
      // it first makes the surviving global row addressable without a
      // workspace, which is what a global DELETE needs.
      const perWorkspace: WorkspaceSuggestions[] = await Promise.all([
        loadSource(GLOBAL_SOURCE_ID, ''),
        ...projects.map((p) => loadSource(p.workspace_id, p.name)),
      ])
      now = Date.now()
      rows = sortSuggestions(mergeSuggestionRows(perWorkspace))
      warnings = suggestionWarnings(perWorkspace)
    } catch (e) {
      toast.error('Failed to load suggestion decisions', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
  })

  async function remove(r: SuggestionRow) {
    removing = key(r)
    try {
      const result = await deleteSuggestion(r.pattern, r.workspace_id, r.scope)
      await load()
      const { title, description } = removeMessage(r.pattern, result)
      toast.success(title, description ? { description } : undefined)
    } catch (e) {
      toast.error('Remove failed', { description: String(e) })
    } finally {
      removing = null
    }
  }
</script>

<!-- Warnings alone are enough to show the section: a store that could not be
     read has to be visible, and hiding the section would report it as
     "nothing is silenced here". -->
{#if loaded && (rows.length > 0 || warnings.length > 0)}
  <section class="mb-8">
    <h2 class="mb-3 flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
      <BellOff class="size-3" />
      Silenced suggestions
    </h2>
    {#if warnings.length > 0}
      <ul class="mb-2 flex flex-col gap-1 text-xs text-muted-foreground">
        {#each warnings as w (w)}
          <li class="flex items-start gap-1.5">
            <TriangleAlert class="mt-0.5 size-3 shrink-0 text-destructive" />
            <span>{w}</span>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="flex flex-col gap-2">
      {#each rows as r (key(r))}
        <Card.Root>
          <Card.Header>
            <div class="flex flex-wrap items-center gap-2">
              <Card.Title class="font-mono text-sm">{r.pattern}</Card.Title>
              <Badge variant="outline">{kindLabel(r.kind)}</Badge>
              <Badge variant="outline">{r.scope}</Badge>
              <Badge variant="secondary">{r.project}</Badge>
              <span class="text-xs text-muted-foreground">{untilLabel(r.snoozed_until, now)}</span>
            </div>
            <Card.Description>{r.synopsis || 'no synopsis recorded'}</Card.Description>
            <Card.Action>
              <Button
                variant="ghost"
                size="sm"
                class="max-sm:px-2 text-muted-foreground hover:text-destructive"
                onclick={() => remove(r)}
                disabled={removing === key(r)}
                title="Allow this suggestion again"
              >
                <Trash2 data-icon="inline-start" />
                <span class="max-sm:sr-only">Remove</span>
              </Button>
            </Card.Action>
          </Card.Header>
        </Card.Root>
      {/each}
    </div>
  </section>
{/if}
