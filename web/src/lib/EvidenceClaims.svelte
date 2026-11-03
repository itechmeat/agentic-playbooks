<script lang="ts">
  // The claims of a node with `evidence:`, one block per attempt's
  // session: each claim with its verdict in the fixed wording, the
  // fragment every citation names next to it (the span, or the start of
  // the record) so a person compares, and the deterministic "worth a
  // look" flags. A claim that does not stand reads "not supported by a
  // cited record". Nothing here says a record supports a claim. Every text
  // arrives redacted and is shown as plain text.
  import { flagLine, packageLine, sessionHead, verdictStands, verdictText, type SessionShown } from './results'

  let { sessions }: { sessions: SessionShown[] } = $props()
</script>

{#if sessions.length}
  <section class="evidence" data-testid="evidence-claims">
    <h4>Claims and citations</h4>
    {#each sessions as s (s.attempt_id)}
      <div class="session" data-testid="evidence-session">
        <div class="head code">{sessionHead(s)}</div>
        {#each s.packages as p (p.view)}
          <div class="muted" data-testid="evidence-package">{packageLine(p)}</div>
        {/each}
        {#each s.problems as problem, i (i)}
          <div class="problem">{problem}</div>
        {/each}
        {#each s.claims as c (c.id)}
          <div class="claim" class:stands={verdictStands(c.verdict)} data-testid="evidence-claim">
            <div class="text">{c.text}</div>
            <div class="verdict" data-testid="evidence-verdict">{verdictText(c.verdict)}</div>
            {#each c.problems as problem, i (i)}
              <div class="problem">{problem}</div>
            {/each}
            {#each c.cites as x, i (i)}
              <div class="cite">
                <span class="code ref">{x.ref}</span>
                {#if x.fragment !== undefined}
                  <pre class="fragment" data-testid="evidence-fragment">{x.fragment}</pre>
                {/if}
              </div>
            {/each}
            {#if c.flags.length}
              <div class="flags" data-testid="evidence-flags">{flagLine(c.flags)}</div>
            {/if}
          </div>
        {/each}
      </div>
    {/each}
  </section>
{/if}

<style>
  /* Plain CSS over the app's variables. */
  .evidence {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }
  h4 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
  }
  .session,
  .claim {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .claim {
    padding-left: 8px;
    border-left: 2px solid var(--destructive, var(--border));
  }
  .claim.stands {
    border-left-color: var(--border);
  }
  .code {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .ref {
    overflow-wrap: anywhere;
  }
  .muted,
  .verdict,
  .flags {
    color: var(--muted-foreground);
  }
  .problem {
    color: var(--destructive, inherit);
  }
  .fragment {
    margin: 2px 0 0;
    padding: 4px 6px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    background: var(--muted);
    border-radius: 4px;
    font-size: 12px;
  }
</style>
