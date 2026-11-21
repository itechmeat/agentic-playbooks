<script lang="ts">
  // The journal tail of one start rule, newest last: every line through
  // `journalLineText`, the twin of the engine's `journal_line_text`, so the
  // page says what `apb trigger log` says. A line may lack every optional
  // field (a `positioned` line has no seq, code, codes or conditions).
  import type { LogEntry } from './api.gen'
  import { journalLineText } from './triggers'

  let { journal }: { journal: LogEntry[] } = $props()

  const fmtTime = (ms: number) => new Date(ms).toLocaleString()
</script>

{#if journal.length}
  <div class="journal">
    <h3>Journal</h3>
    <div class="log" data-testid="trigger-journal">
      {#each journal as line (line.id)}
        <div class="log-line">
          <span class="muted">{fmtTime(line.at)}</span>
          <span class="words">{journalLineText(line)}</span>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .journal {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .log {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    max-height: 20rem;
    overflow: auto;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-md, 6px);
    font-size: 0.75rem;
  }
  .log-line {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.25rem 0.75rem;
  }
  .log-line .words {
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--muted-foreground);
  }
</style>
