<script lang="ts">
  import { fixHash, issueTarget, parseIssue, type AnyIssue, type Drawable, type IssueTarget } from './issues'

  // The validation issues of a playbook or a draft. A click selects and
  // centres the issue's target on the canvas (`onselect`); "fix" opens the
  // editor on it when `fix` names the playbook. A parse error comes first as
  // a document issue, and issues kept from the last valid YAML say so.
  let {
    issues,
    drawable,
    stale = false,
    parseError = null,
    fix = null,
    onselect,
  }: {
    issues: AnyIssue[]
    drawable: Drawable
    stale?: boolean
    parseError?: string | null
    fix?: { workspace: string; id: string } | null
    onselect?: (target: IssueTarget, issue: AnyIssue) => void
  } = $props()

  const rows = $derived([
    ...(parseError ? [{ issue: parseIssue(parseError), parse: true }] : []),
    ...issues.map((issue) => ({ issue, parse: false })),
  ])

  function where(t: IssueTarget): string {
    if (t.kind === 'edge') return t.edges.length > 1 ? `${t.edges.length} edges` : `edge ${t.first}`
    if (t.kind === 'node') return `node ${t.node}`
    return t.yaml ? 'yaml' : 'document'
  }
</script>

{#if rows.length}
  <div class="issues" data-testid="issues-panel">
    {#if stale}
      <p class="stale" data-testid="issues-stale">From the last valid YAML: the current text does not parse.</p>
    {/if}
    <ul>
      {#each rows as row, i (i)}
        {@const t = row.parse ? ({ kind: 'document', yaml: true } as IssueTarget) : issueTarget(row.issue, drawable)}
        <li class={row.issue.severity === 'error' ? 'error' : 'warning'}>
          <button
            type="button"
            class="pick"
            title={t.kind === 'document' && !t.yaml ? 'Nothing on the graph to show' : `Show ${where(t)}`}
            onclick={() => onselect?.(t, row.issue)}
            data-testid="issue-row"
          >
            <span class="code">{row.issue.code}</span>
            <span class="msg">{row.issue.message}</span>
            <span class="where">{where(t)}</span>
          </button>
          {#if fix && !row.parse}
            <a class="fix" href={fixHash(fix.workspace, fix.id, t)} data-testid="issue-fix">fix</a>
          {/if}
        </li>
      {/each}
    </ul>
  </div>
{/if}

<style>
  /* Plain CSS over the app's variables. */
  .issues {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .stale {
    margin: 0;
    color: var(--c-amber);
    font-size: 11px;
  }
  ul {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .pick {
    display: flex;
    flex: 1;
    flex-wrap: wrap;
    gap: 0 6px;
    min-width: 0;
    padding: 2px 0;
    border: 0;
    background: transparent;
    color: var(--c-fg-2);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .pick:hover .msg {
    text-decoration: underline;
  }
  .pick:focus-visible,
  .fix:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 2px;
  }
  .code {
    font-weight: 600;
  }
  .error .code {
    color: var(--c-red);
  }
  .warning .code {
    color: var(--c-amber);
  }
  .msg {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .where {
    color: var(--muted-foreground);
  }
  .fix {
    color: var(--c-violet);
    font-size: 11px;
  }
</style>
