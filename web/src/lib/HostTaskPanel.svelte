<script lang="ts">
  import * as Card from '$lib/components/ui/card'
  import { Badge } from '$lib/components/ui/badge'
  import { deadlineNote, type HostTaskEntry } from './rungates'

  // Host execution mode (0.23.0): the agent steps that wait for the host
  // session. Read-only: the host submits them through MCP run_task_submit
  // (or `apb tasks submit`), never from this page. The prompt texts come
  // verbatim from the run and render as plain text inside a collapsed
  // <details>, so a long prompt does not push the rest of the sidebar away.
  let { tasks, now = Date.now() }: { tasks: HostTaskEntry[]; now?: number } = $props()
</script>

{#if tasks.length}
  <Card.Root class="border-primary/60" data-testid="host-tasks">
    <Card.Header>
      <Card.Title class="text-sm">Host tasks</Card.Title>
    </Card.Header>
    <Card.Content class="flex flex-col gap-3">
      <p class="text-xs text-muted-foreground">
        Waiting for the host session to run them with its own subagents.
      </p>
      {#each tasks as t (`${t.runId}/${t.taskId}`)}
        <div class="flex flex-col gap-1" data-testid="host-task">
          <div class="flex flex-wrap items-center gap-1.5">
            <span class="font-mono text-xs">{t.node}</span>
            <Badge variant="outline" class="h-5 text-[10px]">attempt {t.attempt}</Badge>
            {#if t.modelHint}
              <Badge variant="outline" class="h-5 text-[10px]" title={t.hintNote ?? undefined}>model hint {t.modelHint}</Badge>
            {/if}
          </div>
          {#if t.hintNote}
            <p class="hint-note" data-testid="host-task-hint">{t.hintNote}</p>
          {/if}
          <div class="text-[11px] text-muted-foreground">
            <span class="font-mono">{t.taskId}</span>
            {#if deadlineNote(t.deadline, now)}
              <span> · deadline {deadlineNote(t.deadline, now)}</span>
            {/if}
          </div>
          <details class="text-xs">
            <summary class="cursor-pointer text-muted-foreground">Prompt</summary>
            <pre class="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded bg-muted p-2">{t.prompt}</pre>
          </details>
          {#if t.rolePrompt}
            <details class="text-xs">
              <summary class="cursor-pointer text-muted-foreground">Role prompt</summary>
              <pre class="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words rounded bg-muted p-2">{t.rolePrompt}</pre>
            </details>
          {/if}
        </div>
      {/each}
    </Card.Content>
  </Card.Root>
{/if}

<style>
  /* Plain CSS over the app's variables: the labelled model hint. */
  .hint-note {
    margin: 0;
    font-size: 11px;
    color: var(--muted-foreground);
  }
</style>
