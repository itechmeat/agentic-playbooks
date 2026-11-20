<script lang="ts">
  import { Button } from '$lib/components/ui/button'
  import { authState } from './auth.svelte'
  import { scrollToSnapshot } from './results'
  import { refusalKey, reviewCommand, type ReviewEntry } from './rungates'
  import { initialValues, invalidNumbers, toPayload } from './reviewform'
  import type { BoundSnapshot } from './api.gen'
  import UnattendedNote from './UnattendedNote.svelte'
  import { pilotLine } from './listprogress'

  // The run page's pending human reviews: per gate its guidance, the
  // decision model's recommendation, what an unattended run does there, and
  // one button per option. The gate's `decide_review` next action is drawn
  // here, never in the next-actions panel.
  //
  // A gate with `approves:` shows the snapshots its request is bound to,
  // each linked to its row in the results panel: an approval is a fact
  // about exactly those stored bytes. A person-only gate takes a decision
  // only from a person's channel; a dashboard without keys is not one
  // (anything on this machine reaches it), so there the card offers the
  // terminal command instead of its buttons. `refusals` holds the server's
  // sentence for a decision it refused, keyed by `refusalKey`.
  let {
    reviews,
    posting = null,
    refusals = {},
    projectDir = '',
    projectDirMissing = false,
    onDecide,
  }: {
    reviews: ReviewEntry[]
    /** `<node>:<option>` of the decision in flight. */
    posting?: string | null
    refusals?: Record<string, string>
    /** The run's project directory, where a terminal command must run;
     * `null` while it is still being looked up. */
    projectDir?: string | null
    /** The run belongs to a workspace whose directory could not be found:
     * the command is shown without it, with a note to run it there. */
    projectDirMissing?: boolean
    onDecide: (
    review: ReviewEntry,
    option: string,
    fields?: Record<string, unknown>,
  ) => void | Promise<void>
  } = $props()

  // The values the person typed, per open request (`refusalKey`: run, node
  // and request seq), over the request's rendered defaults: a looping
  // gate's next request starts from its own defaults, never from what was
  // typed for the last one. Only edited fields are held here.
  let edits = $state<Record<string, Record<string, unknown>>>({})
  const fieldValues = (pr: ReviewEntry): Record<string, unknown> => ({
    ...initialValues(pr.fields ?? []),
    ...(edits[refusalKey(pr)] ?? {}),
  })
  function setField(pr: ReviewEntry, name: string, value: unknown) {
    const key = refusalKey(pr)
    edits[key] = { ...(edits[key] ?? {}), [name]: value }
  }
  // Why the panel posted nothing for a request: a number that does not
  // read. Whether a required field has a value is the server's one check:
  // its refusal shows as the request's refusal.
  let formProblems = $state<Record<string, string>>({})
  function decide(pr: ReviewEntry, opt: string) {
    const fields = pr.fields ?? []
    if (!fields.length) {
      void onDecide(pr, opt)
      return
    }
    const key = refusalKey(pr)
    const values = fieldValues(pr)
    const bad = invalidNumbers(fields, values)
    if (bad.length) {
      formProblems[key] = `${bad.join(', ')}: not a number`
      return
    }
    delete formProblems[key]
    void onDecide(pr, opt, toPayload(fields, values, Object.keys(edits[key] ?? {})))
  }
  const requiredLabel = (required: string[] | undefined): string =>
    required?.length ? ` (required for ${required.join(', ')})` : ''

  const authSnapshot = authState()
  // Keyless only once the status read answered: until then the buttons
  // stay (the server refuses a keyless person-only decision anyway).
  const keyless = $derived(authSnapshot.checked && !authSnapshot.required)

  const reasonWord = { not_run: 'not run', no_snapshot: 'no snapshot' } as const

  function snapshotHead(s: BoundSnapshot): string {
    return [s.node, `execution ${s.execution}`, ...(s.item ? [`item ${s.item}`] : [])].join(', ')
  }
  function snapshotFlags(s: BoundSnapshot): string[] {
    return [
      ...(s.incomplete ? ['incomplete'] : []),
      ...(s.redacted ? ['redacted'] : []),
      ...(s.omitted ? [`${s.omitted} omitted`] : []),
    ]
  }
  // An option the gate's `max_scope` withholds above the scope: shown
  // disabled, with the reason as its title and under the buttons.
  function withheldOption(pr: ReviewEntry, opt: string): boolean {
    return pr.withheld?.includes(opt) ?? false
  }
</script>

{#if reviews.length}
  <section class="box glow reviews" style:--k="var(--c-amber)" data-testid="review-panel">
    <span class="box-cap">Human review</span>
    {#each reviews as pr (pr.node)}
      {@const cli = pr.personOnly && keyless}
      <div class="gate">
        <span class="node">{pr.node}</span>
        {#if pr.prompt}
          <pre class="prose-text guidance">{pr.prompt}</pre>
        {/if}
        {#if pr.recommendation}
          <p class="recommendation" data-testid="review-recommendation">{pr.recommendation}</p>
        {/if}
        {#if pr.unattended}
          <UnattendedNote outcome={pr.unattended} testid="review-unattended" />
        {/if}
        {#if pr.pilot && pilotLine(pr.pilot)}
          <p class="scope" data-testid="review-pilot">{pilotLine(pr.pilot)}</p>
        {/if}
        {#if pr.scopeLine}
          <p class="scope" data-testid="review-scope">scope: {pr.scopeLine}</p>
        {/if}
        {#if pr.show?.length}
          <dl class="shown">
            {#each pr.show as se, i (i)}
              <div data-testid="review-shown">
                <dt>{se.label}{se.untrusted ? ' (untrusted text)' : ''}</dt>
                <dd>{se.text}</dd>
              </div>
            {/each}
          </dl>
        {/if}
        {#if pr.fields?.length}
          <div class="fields">
            {#each pr.fields as f (f.name)}
              {@const v = fieldValues(pr)[f.name]}
              <label class="field" data-testid="review-field-input">
                <span class="fname"
                  >{f.name}{f.untrusted ? ' (untrusted text)' : ''}{requiredLabel(f.required)}</span
                >
                {#if f.kind === 'text'}
                  <textarea
                    data-testid="review-field-{f.name}"
                    value={typeof v === 'string' ? v : ''}
                    oninput={(e) => setField(pr, f.name, e.currentTarget.value)}
                  ></textarea>
                {:else if f.kind === 'number'}
                  <!-- A text input: a browser's number input drops what it
                       cannot take ("12abc" reads 12), so the panel reads the
                       text as typed and names it when it is no number. -->
                  <input
                    type="text"
                    inputmode="decimal"
                    data-testid="review-field-{f.name}"
                    value={v === null || v === undefined ? '' : String(v)}
                    oninput={(e) => setField(pr, f.name, e.currentTarget.value)}
                  />
                {:else if f.kind === 'bool' && typeof f.default !== 'boolean'}
                  <!-- A bool without a default has no value until the
                       person chooses one: "not chosen" stays visible and
                       choosable, and is posted as no value. -->
                  <select
                    data-testid="review-field-{f.name}"
                    class:unset={typeof v !== 'boolean'}
                    value={typeof v === 'boolean' ? String(v) : ''}
                    onchange={(e) => {
                      const c = e.currentTarget.value
                      setField(pr, f.name, c === '' ? undefined : c === 'true')
                    }}
                  >
                    <option value="">not chosen</option>
                    <option value="true">true</option>
                    <option value="false">false</option>
                  </select>
                {:else if f.kind === 'bool'}
                  <input
                    type="checkbox"
                    data-testid="review-field-{f.name}"
                    checked={Boolean(v)}
                    onchange={(e) => setField(pr, f.name, e.currentTarget.checked)}
                  />
                {:else}
                  <select
                    data-testid="review-field-{f.name}"
                    value={typeof v === 'string' ? v : ''}
                    onchange={(e) => setField(pr, f.name, e.currentTarget.value)}
                  >
                    <!-- Nothing chosen until the person chooses: an enum
                         without a default never posts its first option. -->
                    <option value="" disabled>choose</option>
                    {#each f.options ?? [] as o, i (i)}
                      <option value={o}>{o}</option>
                    {/each}
                  </select>
                {/if}
              </label>
            {/each}
          </div>
        {/if}
        {#if pr.binding}
          <div class="binding" data-testid="review-binding">
            <p class="note">Approving approves exactly these stored snapshots:</p>
            <ul>
              {#each pr.binding.snapshots as s (s.snapshot_id)}
                <li data-testid="review-binding-snapshot">
                  <span>{snapshotHead(s)}</span>
                  <span class="mono" title={s.snapshot_id}>{s.snapshot_id.slice(0, 12)}</span>
                  {#each snapshotFlags(s) as f (f)}<span class="flag">{f}</span>{' '}{/each}
                  <button
                    type="button"
                    class="link"
                    data-testid="review-binding-link"
                    onclick={() => scrollToSnapshot(s.snapshot_id)}
                  >
                    Show in results
                  </button>
                </li>
              {/each}
              {#each pr.binding.missing ?? [] as m (m.node)}
                <li class="missing" data-testid="review-binding-missing">
                  {m.node}: {reasonWord[m.reason] ?? m.reason}; approving does not approve it
                </li>
              {/each}
            </ul>
            <p class="note mono" data-testid="review-binding-nonce">binding {pr.binding.nonce}</p>
          </div>
        {/if}
        {#if pr.personOnly && !cli}
          <p class="note" data-testid="review-person-only">
            Only a person decides this gate: at a terminal, or here while signed in.
          </p>
        {/if}
        {#if cli}
          <div class="cli" data-testid="review-cli">
            <p>
              Only a person decides this gate, and this dashboard has no keys, so anything on this machine could post
              here. Decide from a terminal, or issue a key with <code class="mono">apb server key issue</code> and sign
              in:
            </p>
            {#if projectDir === null}
              <p class="note" data-testid="review-cli-pending">Looking up the run's project directory...</p>
            {:else}
              {#if projectDirMissing}
                <p class="note" data-testid="review-cli-no-dir">
                  The run's project directory could not be found: run the command from that project.
                </p>
              {/if}
              <!-- A withheld option gets no command: it is listed as
                   withheld below, with its reason. -->
              {#each pr.options.filter((o) => !withheldOption(pr, o)) as opt (opt)}
                <code class="mono" data-testid="review-cli-command">{reviewCommand(pr, opt, projectDir)}</code>
              {/each}
            {/if}
          </div>
        {:else}
          <div class="options">
            {#each pr.options as opt (opt)}
              <Button
                variant="outline"
                size="sm"
                data-testid="review-option"
                onclick={() => decide(pr, opt)}
                disabled={posting === `${pr.node}:${opt}` || withheldOption(pr, opt)}
                title={withheldOption(pr, opt) ? pr.withheldReason : undefined}
              >
                {opt}
              </Button>
            {/each}
          </div>
        {/if}
        {#if pr.withheld?.length}
          {#each pr.withheld as opt (opt)}
            <p class="withheld" data-testid="review-withheld">
              {opt} is withheld: {pr.withheldReason}
            </p>
          {/each}
        {/if}
        {#if formProblems[refusalKey(pr)]}
          <p class="refusal" role="alert" data-testid="review-form-problem">{formProblems[refusalKey(pr)]}</p>
        {/if}
        {#if refusals[refusalKey(pr)]}
          <p class="refusal" role="alert" data-testid="review-refusal">{refusals[refusalKey(pr)]}</p>
        {/if}
      </div>
    {/each}
  </section>
{/if}

<style>
  .reviews {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 16px 12px;
  }
  .gate {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .node {
    color: var(--c-amber);
    font-size: 12px;
  }
  .guidance {
    margin: 0;
    font-size: 14px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .recommendation,
  .note {
    margin: 0;
    color: var(--muted-foreground);
    font-size: 12px;
  }
  .binding ul {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 2px 0;
    padding: 0;
    list-style: none;
    font-size: 12px;
  }
  .binding li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 6px;
    min-width: 0;
  }
  .binding .missing {
    color: var(--c-red);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
    overflow-wrap: anywhere;
  }
  .flag {
    padding: 0 4px;
    border: 1px solid var(--c-amber);
    border-radius: 3px;
    color: var(--c-amber);
    font-size: 10px;
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--primary);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  .cli {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .cli p {
    margin: 0;
  }
  .cli code {
    display: block;
    padding: 4px 6px;
    border-radius: 4px;
    background: var(--muted);
    font-size: 11px;
  }
  .refusal {
    margin: 0;
    color: var(--c-red);
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .options {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .options :global(button) {
    height: 28px;
  }
  .scope {
    margin: 0;
    color: var(--muted-foreground);
    font-size: 12px;
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .withheld {
    margin: 0;
    color: var(--c-amber);
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .shown {
    margin: 2px 0;
    font-size: 12px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .shown div {
    display: flex;
    gap: 6px;
    min-width: 0;
  }
  .shown dt {
    color: var(--muted-foreground);
    white-space: nowrap;
  }
  .shown dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
  }
  .field .fname {
    color: var(--muted-foreground);
    min-width: 10ch;
  }
  .field textarea,
  .field input[inputmode='decimal'],
  .field select {
    flex: 1;
    min-width: 0;
    padding: 4px 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--background);
    color: var(--foreground);
    font: inherit;
    font-size: 12px;
  }
  .field select.unset {
    color: var(--muted-foreground);
    border-style: dashed;
  }
  .field textarea {
    min-height: 3em;
    resize: vertical;
  }
</style>
