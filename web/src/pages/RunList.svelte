<script lang="ts">
  import { fetchRuns } from '../lib/api'
  import { subscribeChanges } from '../lib/ws'
  import type { RunSummary } from '../lib/types'
  import Topbar from '$lib/components/Topbar.svelte'
  import { Badge } from '$lib/components/ui/badge'
  import * as Table from '$lib/components/ui/table'
  import * as Empty from '$lib/components/ui/empty'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { runStatusClass } from '../lib/status'
  import { toast } from 'svelte-sonner'
  import PlayCircle from '@lucide/svelte/icons/play-circle'

  let items = $state<RunSummary[]>([])
  let loaded = $state(false)

  async function load() {
    try {
      items = await fetchRuns()
    } catch (e) {
      toast.error('Failed to load runs', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })

  const statusVariant = (s: string) =>
    runStatusClass(s) ? 'outline' : ('secondary' as const)
</script>

<Topbar active="runs" />

<div class="min-h-0 flex-1 overflow-auto">
  <div class="mx-auto w-full max-w-4xl px-4 py-6">
    {#if !loaded}
      <div class="flex flex-col gap-2">
        {#each Array(4) as _, i (i)}<Skeleton class="h-10 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><PlayCircle /></Empty.Media>
          <Empty.Title>No runs yet</Empty.Title>
          <Empty.Description>Start a playbook to see runs here.</Empty.Description>
        </Empty.Header>
      </Empty.Root>
    {:else}
      <div class="rounded-lg border border-border">
        <Table.Root>
          <Table.Header>
            <Table.Row>
              <Table.Head>Run</Table.Head>
              <Table.Head>Playbook</Table.Head>
              <Table.Head>Status</Table.Head>
              <Table.Head>Project</Table.Head>
            </Table.Row>
          </Table.Header>
          <Table.Body>
            {#each items as r (`${r.workspace_id}/${r.run_id}`)}
              <Table.Row>
                <Table.Cell class="font-mono text-xs">
                  <a
                    href={`#/run/${encodeURIComponent(r.workspace_id)}/${encodeURIComponent(r.run_id)}`}
                    class="font-medium text-foreground hover:underline"
                  >
                    {r.run_id}
                  </a>
                </Table.Cell>
                <Table.Cell>{r.playbook}</Table.Cell>
                <Table.Cell>
                  <Badge variant={statusVariant(r.status)} class={runStatusClass(r.status)}>
                    {r.status}
                  </Badge>
                </Table.Cell>
                <Table.Cell class="text-muted-foreground">{r.project ?? ''}</Table.Cell>
              </Table.Row>
            {/each}
          </Table.Body>
        </Table.Root>
      </div>
    {/if}
  </div>
</div>
