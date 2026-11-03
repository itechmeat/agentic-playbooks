<script lang="ts">
  // The claims an attempt's evidence session checked: per claim its
  // verdict with the fixed wording (never "confirmed"), the claim text,
  // the flags of a weak but valid citation as "worth a look", the problems
  // of an unsupported one, and each cited fragment next to its reference so
  // a person compares them. Every text is plain text; nothing is rendered
  // as markup.
  import {
    FLAGS_WORDING,
    budgetLine,
    claimsInOrder,
    collectorLine,
    evidenceHead,
    flagLabel,
    verdictLabel,
    verdictTone,
    type EvidenceRow,
  } from './results'

  let { rows }: { rows: EvidenceRow[] } = $props()
</script>

{#each rows as r (r.session)}
  <section class="evidence" data-testid="evidence-row">
    <h4>Evidence · {evidenceHead(r)}</h4>
    <p class="muted">
      {budgetLine(r)}{#if !r.checked}; claims not checked{/if}
    </p>
    {#if !r.closed}<p class="problem" data-testid="evidence-open">session still open: it serves its attempt only</p>{/if}
    {#if r.output_unsupported}<p class="problem" data-testid="evidence-output-unsupported">the attempt's output was not backed by a claims list</p>{/if}
    {#each r.views as v (v.id)}
      {@const line = collectorLine(v)}
      {#if line}<p class="muted" data-testid="evidence-collector">{line}</p>{/if}
    {/each}
    {#each r.problems as p, i (i)}
      <p class="problem" data-testid="evidence-problem">{p}</p>
    {/each}
    <ul class="claims">
      {#each claimsInOrder(r.claims) as c, i (i)}
        <li class="claim" data-testid="evidence-claim">
          <div class="head">
            <span class="code">{c.id}</span>
            <span class="badge {verdictTone(c.verdict)}" data-testid="evidence-verdict">{verdictLabel(c.verdict)}</span>
          </div>
          {#if c.text}<p class="text" data-testid="evidence-claim-text">{c.text}</p>{/if}
          <p class="muted" data-testid="evidence-wording">{c.wording}</p>
          {#if c.absence?.length}
            <p class="muted" data-testid="evidence-absence">Searched: {c.absence.join(', ')}</p>
          {/if}
          {#if c.flags.length}
            <p class="flags" data-testid="evidence-flags">
              {FLAGS_WORDING}: {c.flags.map(flagLabel).join(', ')}
            </p>
          {/if}
          {#each c.problems as p, i (i)}
            <p class="problem">{p}</p>
          {/each}
          {#each c.cites as f, i (i)}
            <div class="cite">
              <span class="code ref">{f.ref}</span>
              <blockquote class="fragment" data-testid="evidence-fragment">{f.fragment}</blockquote>
            </div>
          {/each}
        </li>
      {/each}
    </ul>
  </section>
{/each}

<style>
  /* Plain CSS over the app's variables. */
  .evidence {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  h4 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .problem {
    color: var(--destructive);
    overflow-wrap: anywhere;
  }
  .claims {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .claim {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-left: 8px;
    border-left: 2px solid var(--border);
    min-width: 0;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
  }
  .code {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .ref {
    color: var(--muted-foreground);
    overflow-wrap: anywhere;
  }
  .text {
    overflow-wrap: anywhere;
  }
  .badge {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 1px 8px;
  }
  .badge.bad {
    border-color: var(--destructive);
    color: var(--destructive);
  }
  .badge.warn {
    border-color: var(--c-amber, var(--border));
    color: var(--c-amber, inherit);
  }
  .flags {
    color: var(--c-amber, var(--muted-foreground));
  }
  .cite {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .fragment {
    margin: 0;
    padding: 4px 8px;
    border-left: 2px solid var(--primary);
    background: var(--muted, transparent);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
