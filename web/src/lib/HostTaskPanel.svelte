<script lang="ts">
  import { deadlineNote, type HostTaskEntry } from './rungates'

  // Host execution mode (0.23.0): the agent steps that wait for the host
  // session. Read-only: the host submits them through MCP run_task_submit
  // (or `apb tasks submit`), never from this page. The prompt texts come
  // verbatim from the run and render as plain text inside a collapsed
  // <details>, so a long prompt does not push the rest of the sidebar away.
  let { tasks, now = Date.now() }: { tasks: HostTaskEntry[]; now?: number } = $props()
</script>

{#if tasks.length}
  <section class="box flex flex-col gap-3 px-4 pt-4 pb-3" style:--k="var(--c-teal)" data-testid="host-tasks">
    <span class="box-cap">Host tasks</span>
      <p class="text-xs text-muted-foreground">
        Waiting for the host session to run them with its own subagents.
      </p>
      <!-- The engine's execution contract, the same on every task: shown once,
           folded so it does not push the rest of the column away. -->
      <details class="text-xs">
        <summary class="cursor-pointer text-muted-foreground">How the host runs them</summary>
        <p class="execution-note mt-1" data-testid="host-task-contract">{tasks[0].executionNote}</p>
      </details>
      {#each tasks as t (`${t.runId}/${t.taskId}`)}
        <div class="flex flex-col gap-1" data-testid="host-task">
          <div class="flex flex-wrap items-center gap-1.5">
            <span class="text-xs text-teal">{t.node}</span>
            <span class="tag text-muted-foreground">attempt {t.attempt}</span>
            {#if t.modelHint}
              <span class="tag text-muted-foreground" title={t.hintNote ?? undefined}>model hint {t.modelHint}</span>
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
            <pre class="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-words border border-line bg-field p-2 text-fg-2">{t.prompt}</pre>
          </details>
          {#if t.rolePrompt}
            <details class="text-xs">
              <summary class="cursor-pointer text-muted-foreground">Role prompt</summary>
              <pre class="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words border border-line bg-field p-2 text-fg-2">{t.rolePrompt}</pre>
            </details>
          {/if}
        </div>
      {/each}
  </section>
{/if}

<style>
  /* Plain CSS over the app's variables: the labelled model hint. */
  .hint-note {
    margin: 0;
    font-size: 11px;
    color: var(--muted-foreground);
  }
  /* The execution contract: how the host runs every task. */
  .execution-note {
    margin: 0;
    font-size: 11px;
    line-height: 1.4;
    color: var(--foreground);
  }
</style>
