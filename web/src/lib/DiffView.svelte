<script lang="ts">
  import { fetchDiff } from './api'
  import { diffEdgeChanges, diffOrderChanges, formatDiff } from './difffmt'
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
  // The edges present in both versions whose attributes changed, and the
  // sources whose edges are checked in a different order.
  const edgeChanges = $derived(diffEdgeChanges(diff))
  const orderChanges = $derived(diffOrderChanges(diff))

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
      ? 'text-green'
      : kind === 'del'
        ? 'text-red'
        : kind === 'meta'
          ? 'text-violet'
          : 'text-fg-2'
</script>

<div class="flex h-full min-h-0 flex-col gap-3">
  <div class="flex flex-shrink-0 flex-wrap items-center gap-2 text-sm">
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
    {#if error}<span class="text-sm text-red" title={error}>error</span>{/if}
  </div>

  {#if diff}
    <div class="box flex flex-shrink-0 flex-col gap-2 px-3 py-2 text-xs">
      {#if diff.nodes_added.length}
        <div>
          <h4 class="text-[11px] tracking-[0.08em] text-muted-foreground uppercase">nodes added</h4>
          <ul class="pl-3.5">{#each diff.nodes_added as n (n)}<li class="text-green">+ {n}</li>{/each}</ul>
        </div>
      {/if}
      {#if diff.nodes_removed.length}
        <div>
          <h4 class="text-[11px] tracking-[0.08em] text-muted-foreground uppercase">nodes removed</h4>
          <ul class="pl-3.5">{#each diff.nodes_removed as n (n)}<li class="text-red">- {n}</li>{/each}</ul>
        </div>
      {/if}
      {#if diff.nodes_changed.length}
        <div>
          <h4 class="text-[11px] tracking-[0.08em] text-muted-foreground uppercase">nodes changed</h4>
          <ul class="pl-3.5">{#each diff.nodes_changed as n (n)}<li class="text-amber">~ {n}</li>{/each}</ul>
        </div>
      {/if}
      {#if diff.edges_added.length}
        <div>
          <h4 class="text-[11px] tracking-[0.08em] text-muted-foreground uppercase">edges added</h4>
          <ul class="pl-3.5">{#each diff.edges_added as e (e)}<li class="text-green">+ {e}</li>{/each}</ul>
        </div>
      {/if}
      {#if diff.edges_removed.length}
        <div>
          <h4 class="text-[11px] tracking-[0.08em] text-muted-foreground uppercase">edges removed</h4>
          <ul class="pl-3.5">{#each diff.edges_removed as e (e)}<li class="text-red">- {e}</li>{/each}</ul>
        </div>
      {/if}
      {#if edgeChanges.length}
        <section class="edge-list" data-testid="diff-edges-changed">
          <h4>Edges changed</h4>
          <ul>
            {#each edgeChanges as c, i (i)}
              <li>
                <span class="addr">{c.before}</span>
                {#if c.after !== c.before}<span class="arrow">→</span><span class="addr">{c.after}</span>{/if}
                <span class="what">{c.changed.join(', ')}</span>
              </li>
            {/each}
          </ul>
        </section>
      {/if}
      {#if orderChanges.length}
        <section class="edge-list" data-testid="diff-order-changed">
          <h4>Order changed</h4>
          <ul>
            {#each orderChanges as c, i (i)}
              <li>
                <span class="node">{c.node}</span>
                <span class="addr">{c.before.join(', ')}</span>
                <span class="arrow">→</span>
                <span class="addr">{c.after.join(', ')}</span>
              </li>
            {/each}
          </ul>
        </section>
      {/if}
    </div>

    <pre
      class="min-h-0 flex-1 overflow-auto border border-line bg-field p-3 font-mono text-xs leading-relaxed">{#each lines as line (line)}<span
          class={lineClass(line.kind)}>{line.kind === 'add' ? '+' : line.kind === 'del' ? '-' : ' '}{line.text}{'\n'}</span>{/each}</pre>
  {/if}
</div>

<style>
  /* Plain CSS over the app's variables. */
  .edge-list h4 {
    margin: 0;
    font-size: 11px;
    font-weight: normal;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted-foreground);
  }
  .edge-list ul {
    margin: 0;
    padding-left: 14px;
    list-style: none;
  }
  .edge-list li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 6px;
    color: var(--c-amber);
    overflow-wrap: anywhere;
  }
  .addr {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .arrow,
  .what {
    color: var(--muted-foreground);
  }
  .node {
    font-weight: 600;
  }
</style>
