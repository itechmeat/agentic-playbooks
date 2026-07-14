<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte'
  let { data }: { data: { title: string; kind: string; status?: string } } = $props()

  // start - entry point, must have no incoming edge;
  // finish - terminal, must have no outgoing edge.
  const hasTarget = $derived(data.kind !== 'start')
  const hasSource = $derived(data.kind !== 'finish')
</script>

<div class="playbook-node" data-kind={data.kind} data-status={data.status ?? ''}>
  {#if hasTarget}<Handle type="target" position={Position.Left} />{/if}
  <span class="kind">{data.kind}</span>
  <strong>{data.title}</strong>
  {#if data.status}<span class="status">{data.status}</span>{/if}
  {#if hasSource}<Handle type="source" position={Position.Right} />{/if}
</div>

<style>
  .playbook-node {
    padding: 8px 12px;
    border: 1px solid #8884;
    border-radius: 8px;
    background: #ffffff;
    color: #1a1a1a;
    min-width: 160px;
  }
  @media (prefers-color-scheme: dark) {
    .playbook-node { background: #242430; color: #e6e6e6; border-color: #3a3a44; }
  }
  .kind { display: block; font-size: 11px; opacity: 0.6; }
  .status { display: block; font-size: 11px; margin-top: 2px; opacity: 0.8; }
  [data-kind='condition'] { border-style: dashed; }
  [data-status='running'] { border-color: #2563eb; box-shadow: 0 0 0 2px #2563eb55; }
  [data-status='succeeded'] { border-color: #22a06b; }
  [data-status='failed'], [data-status='timed_out'] { border-color: #dc2626; }
  [data-status='interrupted'], [data-status='unknown'] { border-color: #d97706; }
</style>
