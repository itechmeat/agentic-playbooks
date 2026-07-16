<script lang="ts">
  import { fetchDiff } from './api'
  import { formatDiff } from './difffmt'
  import type { VersionDiff } from './types'
  import { Button } from '$lib/components/ui/button'
  import { Spinner } from '$lib/components/ui/spinner'
  import * as Select from '$lib/components/ui/select'
  import ArrowRight from '@lucide/svelte/icons/arrow-right'

  let { id, versions, workspace = '' }: { id: string; versions: string[]; workspace?: string } =
    $props()

  let from = $state('')
  let to = $state('')
  let diff = $state<VersionDiff | null>(null)
  let loading = $state(false)
  let error = $state<string | null>(null)

  const lines = $derived(diff ? formatDiff(diff.yaml_diff) : [])

  $effect(() => {
    if (versions.length >= 2) {
      from = versions[versions.length - 2]
      to = versions[versions.length - 1]
    } else if (versions.length === 1) {
      from = versions[0]
      to = versions[0]
    }
  })

  async function load() {
    if (!from || !to || from === to) {
      diff = null
      return
    }
    loading = true
    error = null
    try {
      diff = await fetchDiff(id, from, to, workspace)
    } catch (e) {
      error = String(e)
      diff = null
    } finally {
      loading = false
    }
  }

  const lineClass = (kind: string) =>
    kind === 'add'
      ? 'text-success'
      : kind === 'del'
        ? 'text-destructive'
        : kind === 'meta'
          ? 'text-muted-foreground'
          : 'text-foreground/80'
</script>

<div class="flex h-full min-h-0 flex-col gap-3">
  <div class="flex flex-shrink-0 items-center gap-2 text-sm">
    <Select.Root type="single" bind:value={from}>
      <Select.Trigger class="h-8 w-28">{from || 'from'}</Select.Trigger>
      <Select.Content>
        <Select.Group>
          {#each versions as v (v)}<Select.Item value={v} label={v}>{v}</Select.Item>{/each}
        </Select.Group>
      </Select.Content>
    </Select.Root>
    <ArrowRight class="size-4 text-muted-foreground" />
    <Select.Root type="single" bind:value={to}>
      <Select.Trigger class="h-8 w-28">{to || 'to'}</Select.Trigger>
      <Select.Content>
        <Select.Group>
          {#each versions as v (v)}<Select.Item value={v} label={v}>{v}</Select.Item>{/each}
        </Select.Group>
      </Select.Content>
    </Select.Root>
    <Button variant="outline" size="sm" onclick={load} disabled={loading || !from || !to}>
      {#if loading}<Spinner data-icon="inline-start" />{/if}
      diff
    </Button>
    {#if error}<span class="text-sm text-destructive" title={error}>error</span>{/if}
  </div>

  {#if diff}
    <div class="flex flex-shrink-0 flex-col gap-2 text-sm">
      {#if diff.nodes_added.length}
        <div>
          <h4 class="text-[11px] uppercase text-muted-foreground">nodes added</h4>
          <ul class="pl-3.5">{#each diff.nodes_added as n (n)}<li class="text-success">+ {n}</li>{/each}</ul>
        </div>
      {/if}
      {#if diff.nodes_removed.length}
        <div>
          <h4 class="text-[11px] uppercase text-muted-foreground">nodes removed</h4>
          <ul class="pl-3.5">{#each diff.nodes_removed as n (n)}<li class="text-destructive">- {n}</li>{/each}</ul>
        </div>
      {/if}
      {#if diff.nodes_changed.length}
        <div>
          <h4 class="text-[11px] uppercase text-muted-foreground">nodes changed</h4>
          <ul class="pl-3.5">{#each diff.nodes_changed as n (n)}<li class="text-warning">~ {n}</li>{/each}</ul>
        </div>
      {/if}
      {#if diff.edges_added.length}
        <div>
          <h4 class="text-[11px] uppercase text-muted-foreground">edges added</h4>
          <ul class="pl-3.5">{#each diff.edges_added as e (e)}<li class="text-success">+ {e}</li>{/each}</ul>
        </div>
      {/if}
      {#if diff.edges_removed.length}
        <div>
          <h4 class="text-[11px] uppercase text-muted-foreground">edges removed</h4>
          <ul class="pl-3.5">{#each diff.edges_removed as e (e)}<li class="text-destructive">- {e}</li>{/each}</ul>
        </div>
      {/if}
    </div>

    <pre
      class="min-h-0 flex-1 overflow-auto rounded-md border border-border bg-background p-3 font-mono text-xs leading-relaxed">{#each lines as line (line)}<span
          class={lineClass(line.kind)}>{line.kind === 'add' ? '+' : line.kind === 'del' ? '-' : ' '}{line.text}{'\n'}</span>{/each}</pre>
  {/if}
</div>
