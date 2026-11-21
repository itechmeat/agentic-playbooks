<script lang="ts">
  // Start rules: every rule of this machine's projects, a rule's panel with
  // its YAML, its approval, the dry test and the journal tail. A rule is a
  // person's to write and approve: the changes here go through the session
  // routes only, so a dashboard without keys shows them disabled. No passkey
  // is involved; the test reads and writes nothing and runs on any channel.
  import { parse, stringify } from 'yaml'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import CodeEditor from '$lib/CodeEditor.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'
  import { Spinner } from '$lib/components/ui/spinner'
  import * as AlertDialog from '$lib/components/ui/alert-dialog'
  import { toast } from 'svelte-sonner'
  import { authState } from '$lib/auth.svelte'
  import {
    approveTrigger,
    disableTrigger,
    enableTrigger,
    fetchTrigger,
    fetchTriggerApproval,
    fetchTriggers,
    removeTrigger,
    ruleSource,
    setTrigger,
    testTrigger,
    verdictWords,
  } from '$lib/triggers'
  import TriggerJournal from '$lib/TriggerJournal.svelte'
  import type { ApprovalNeed, Rule, RuleView, TestReport, TriggerDetail } from '$lib/api.gen'

  let rules = $state<RuleView[]>([])
  let loadError = $state<string | null>(null)
  let loaded = $state(false)
  // The rule whose panel is open, by name.
  let selected = $state<string | null>(null)
  let detail = $state<TriggerDetail | null>(null)
  let detailError = $state<string | null>(null)
  // The YAML draft of the open rule's document, and why a save cannot run.
  let yamlDraft = $state('')
  let saveError = $state<string | null>(null)
  let busy = $state<string | null>(null)
  // The approval: the exact need the server showed, posted back unchanged.
  let need = $state<ApprovalNeed | null>(null)
  let approveOpen = $state(false)
  let removeOpen = $state(false)
  // The dry test: the event seq to test (empty: the newest stored event).
  let testSeq = $state('')
  let report = $state<TestReport | null>(null)
  let testError = $state<string | null>(null)

  const authSnapshot = authState()
  // Keyless only once the status read answered: until then the buttons stay
  // (the server refuses a keyless write anyway).
  const keyless = $derived(authSnapshot.checked && !authSnapshot.required)
  const WRITE_REASON =
    'A dashboard without keys cannot change a rule: setting, approving, enabling, disabling and removing take a signed-in session.'
  const view = $derived(selected ? (rules.find((r) => r.name === selected) ?? null) : null)

  async function load() {
    try {
      rules = await fetchTriggers()
      loadError = null
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e)
    } finally {
      loaded = true
    }
    // The open panel re-reads with the list, keeping its place.
    if (selected) await open(selected, false)
  }

  $effect(() => {
    void load()
  })

  /** Opens a rule's panel: its view, document and journal tail. A refresh
   * keeps the draft, which may hold edits the stored rule does not. */
  async function open(name: string, resetDraft = true) {
    try {
      const d = await fetchTrigger(name)
      detail = d
      detailError = null
      if (resetDraft) yamlDraft = stringify(d.rule)
      saveError = null
    } catch (e) {
      if (resetDraft) {
        detailError = message(e)
        toast.error(`Could not open rule ${name}`, { description: detailError })
      }
    }
  }

  function pick(name: string) {
    selected = name
    detail = null
    detailError = null
    yamlDraft = ''
    saveError = null
    report = null
    testError = null
    void open(name)
  }

  /** Parses the draft and stores it. The document may not name `enabled`:
   * a rule is enabled or disabled with Enable and Disable only. */
  async function save() {
    if (!selected || busy) return
    let doc: Rule
    try {
      doc = parse(yamlDraft) as Rule
    } catch (e) {
      saveError = `The rule is not valid YAML: ${message(e)}`
      return
    }
    if (!doc || typeof doc !== 'object') {
      saveError = 'The rule is empty.'
      return
    }
    if ('enabled' in doc) {
      saveError = 'A rule names no enabled: enable or disable it with the buttons.'
      return
    }
    busy = 'save'
    try {
      await setTrigger(selected, doc)
      toast.success(`Saved rule ${selected}`, {
        description: 'It starts nothing until it is approved for this version.',
      })
    } catch (e) {
      toast.error(`Could not save rule ${selected}`, { description: message(e) })
      return
    } finally {
      busy = null
    }
    await load()
  }

  /** Shows the exact need for the rule as it stands now; the dialog posts it
   * back unchanged, and the server refuses what changed in between. */
  async function askApproval() {
    if (!selected || busy) return
    busy = 'approve-show'
    let shown: ApprovalNeed
    try {
      shown = await fetchTriggerApproval(selected)
    } catch (e) {
      toast.error(`Could not read the approval for ${selected}`, { description: message(e) })
      return
    } finally {
      busy = null
    }
    need = shown
    approveOpen = true
  }

  async function approve() {
    if (!selected || !need || busy) return
    busy = 'approve'
    try {
      await approveTrigger(selected, need)
    } catch (e) {
      toast.error(`Could not approve ${selected}`, { description: message(e) })
      return
    } finally {
      busy = null
    }
    toast.success(`Approved ${selected}`, { description: 'The next pass positions it at the head.' })
    approveOpen = false
    need = null
    await load()
  }

  async function setEnabled(enabled: boolean) {
    if (!selected || busy) return
    busy = enabled ? 'enable' : 'disable'
    try {
      if (enabled) await enableTrigger(selected)
      else await disableTrigger(selected)
    } catch (e) {
      toast.error(`Could not ${enabled ? 'enable' : 'disable'} ${selected}`, {
        description: message(e),
      })
      return
    } finally {
      busy = null
    }
    await load()
  }

  async function remove() {
    if (!selected || busy) return
    busy = 'remove'
    let answer: { state_left?: string; journal_left?: string } = {}
    try {
      answer = await removeTrigger(selected)
    } catch (e) {
      toast.error(`Could not remove ${selected}`, { description: message(e) })
      return
    } finally {
      busy = null
    }
    // What the removal could not delete beneath the anchor: the rule is
    // gone either way, and the toast names what stays behind.
    const left: string[] = []
    if (answer.state_left) left.push(`state_left: ${answer.state_left}`)
    if (answer.journal_left) left.push(`journal_left: ${answer.journal_left}`)
    toast.success(`Removed ${selected}`, {
      description: left.length
        ? `Something stayed behind: ${left.join('; ')}`
        : 'Its state and journal went with it.',
    })
    removeOpen = false
    selected = null
    detail = null
    yamlDraft = ''
    report = null
    await load()
  }

  /** The dry test: every check a start would run before preparation, over
   * the newest stored event or the seq typed below. Writes nothing. */
  async function runTest() {
    if (!selected || busy) return
    const seq = testSeq.trim() === '' ? undefined : Number(testSeq)
    if (seq !== undefined && (!Number.isFinite(seq) || seq < 0 || !Number.isInteger(seq))) {
      testError = 'The event seq is a whole number of 0 or more, or empty for the newest stored event.'
      return
    }
    busy = 'test'
    testError = null
    try {
      report = await testTrigger(selected, seq)
    } catch (e) {
      testError = message(e)
      return
    } finally {
      busy = null
    }
  }

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e)
  }

  const REFUSAL_WORD: Record<string, string> = {
    passed: 'passed',
    not_run: 'not run',
    refused: 'refused',
  }

  function failureText(f: NonNullable<TestReport['params'][number]['failure']>): string {
    switch (f.kind) {
      case 'missing':
        return `found nothing at ${f.path}`
      case 'not_scalar':
        return `${f.path} is not a scalar`
      case 'too_long':
        return 'the rendered value is too long'
      case 'invalid':
        return 'the template does not parse'
    }
  }

  function keyUseText(u: NonNullable<TestReport['key_use']>): string {
    switch (u.state) {
      case 'free':
        return 'the key is free'
      case 'used':
        return `a run holds this key (${u.run_id}${u.conflict ? ', with different params' : ''})`
      case 'busy':
        return 'another start holds the key right now'
    }
  }

  /** Every check passed and the event matched: the one sentence a test may
   * say, naming what only preparation and the drive decide. */
  const testPassed = $derived(
    report !== null &&
      report.filter.verdict === 'matched' &&
      !report.checks.some((c) => c.state === 'refused'),
  )

</script>

<Topbar active="triggers">
  {#snippet title()}
    <span class="page-title">Triggers</span>
  {/snippet}
  {#snippet actions()}
    <Button variant="ghost" size="sm" onclick={() => void load()}>Refresh</Button>
  {/snippet}
</Topbar>

<PageScroll>
  <div class="page" data-testid="trigger-page">
    <p class="muted intro">
      A start rule watches an inbox account and starts a playbook when an event matches. Every
      rule starts nothing until a person approves it for the exact playbook, launch policy and
      billing decision it names, and a rule's run is always unattended.
    </p>

    {#if keyless}
      <p class="note" data-testid="trigger-keyless-note">{WRITE_REASON}</p>
    {/if}
    {#if loadError}
      <p class="error" data-testid="trigger-load-error">{loadError}</p>
    {/if}

    <section class="list" data-testid="trigger-list">
      <h2>Rules</h2>
      {#if loaded && !rules.length}
        <p class="muted" data-testid="trigger-empty">
          No rules yet: save one with <span class="mono">apb trigger set</span>.
        </p>
      {/if}
      {#each rules as r (r.name)}
        <div class="row" class:open={r.name === selected} data-testid="trigger-row">
          <button type="button" class="row-main" onclick={() => pick(r.name)}>
            <span class="name">{r.name}</span>
            <span class="muted">{r.source}</span>
            <span class="muted mono">{r.playbook} {r.version} ({r.scope})</span>
            <span class="muted">{r.starts_last_hour} started in the last hour</span>
            {#if r.last}
              <span class="muted">last: {verdictWords(r.last)}</span>
            {/if}
          </button>
          <span class="badges">
            {#if r.enabled}
              <span class="badge on">enabled</span>
            {:else}
              <span class="badge">disabled</span>
            {/if}
            {#if !r.approved}
              <span class="badge warn" data-testid="trigger-not-approved">not approved: starts nothing</span>
            {:else if r.needs_approval}
              <span class="badge warn">needs approval</span>
            {:else}
              <span class="badge ok">approved</span>
            {/if}
            {#if r.journal_error}
              <span class="badge warn" data-testid="trigger-journal-refused" title={r.journal_error}>
                stopped: journal refused
              </span>
            {/if}
          </span>
        </div>
      {/each}
    </section>

    {#if selected && view}
      <section class="panel" data-testid="trigger-detail">
        <header class="panel-head">
          <h2>{view.name}</h2>
          <span class="muted mono">{view.incarnation}</span>
          <span class="muted">positioned through event {view.seen_through}</span>
          {#if !view.approved}
            <span class="warn-line">not approved: starts nothing</span>
          {/if}
        </header>

        {#if view.journal_error}
          <div class="issues">
            <p class="issue warning" data-testid="trigger-journal-refused-detail">
              {view.journal_error}
            </p>
          </div>
        {/if}

        {#if view.issues.length}
          <div class="issues" data-testid="trigger-issues">
            {#each view.issues as iss, i (i)}
              <p class="issue" class:warning={iss.severity !== 'error'}>
                <span class="mono">{iss.code}</span> {iss.message}
              </p>
            {/each}
          </div>
        {/if}

        <div class="editor">
          <h3>Rule</h3>
          <div class="editor-box">
            <CodeEditor value={yamlDraft} onChange={(v) => (yamlDraft = v)} language="yaml" />
          </div>
          {#if saveError}
            <p class="error" data-testid="trigger-save-error">{saveError}</p>
          {/if}
          <div class="actions">
            <Button
              size="sm"
              disabled={keyless || busy !== null}
              title={keyless ? WRITE_REASON : undefined}
              onclick={() => void save()}
              data-testid="trigger-save"
            >
              {#if busy === 'save'}<Spinner />{/if}
              Save
            </Button>
          </div>
        </div>

        <div class="actions" data-testid="trigger-actions">
          <Button
            size="sm"
            variant="outline"
            disabled={keyless || busy !== null}
            title={keyless ? WRITE_REASON : undefined}
            onclick={() => void askApproval()}
            data-testid="trigger-approve"
          >
            {#if busy === 'approve-show' || busy === 'approve'}<Spinner />{/if}
            Approve
          </Button>
          {#if view.enabled}
            <Button
              size="sm"
              variant="outline"
              disabled={keyless || busy !== null}
              title={keyless ? WRITE_REASON : undefined}
              onclick={() => void setEnabled(false)}
              data-testid="trigger-disable"
            >
              Disable
            </Button>
          {:else}
            <Button
              size="sm"
              variant="outline"
              disabled={keyless || busy !== null}
              title={keyless ? WRITE_REASON : undefined}
              onclick={() => void setEnabled(true)}
              data-testid="trigger-enable"
            >
              Enable
            </Button>
          {/if}
          <Button
            size="sm"
            variant="ghost"
            disabled={keyless || busy !== null}
            title={keyless ? WRITE_REASON : undefined}
            onclick={() => (removeOpen = true)}
            data-testid="trigger-remove"
          >
            Remove
          </Button>
        </div>

        <div class="test">
          <h3>Test</h3>
          <p class="muted">
            Runs the checks a start would run before preparation, over the newest stored event of
            the rule's source{detail ? ` (${ruleSource(detail.rule)})` : ''} or of the seq below.
            Nothing is written and no run starts.
          </p>
          <div class="test-row">
            <label class="seq">
              Event seq
              <Input
                type="text"
                inputmode="numeric"
                placeholder="newest"
                bind:value={testSeq}
                aria-label="Event seq"
              />
            </label>
            <Button size="sm" disabled={busy !== null} onclick={() => void runTest()} data-testid="trigger-test-run">
              {#if busy === 'test'}<Spinner />{/if}
              Run test
            </Button>
          </div>
          {#if testError}
            <p class="error" data-testid="trigger-test-error">{testError}</p>
          {/if}
          {#if report}
            <div class="report" data-testid="trigger-report">
              {#if report.filter.verdict === 'matched'}
                <p>The event matches the rule's filter.</p>
              {:else}
                <p class="warn-line">
                  skipped: {report.filter.code}{report.filter.name ? ` (${report.filter.name})` : ''}
                </p>
              {/if}
              {#if report.conditions.length}
                <ul class="conditions">
                  {#each report.conditions as c, i (i)}
                    <li data-testid="trigger-condition" class:failed={!c.passed}>
                      <span class="mono">#{i}</span> {c.condition}
                      {c.passed ? 'passed' : 'failed'}{c.got ? ` (type ${c.got})` : ''}
                    </li>
                  {/each}
                </ul>
              {/if}
              {#if report.params.length}
                <ul class="report-list">
                  {#each report.params as p (p.name)}
                    <li>
                      <span class="mono">{p.name}</span>
                      {#if p.failure}
                        {failureText(p.failure)}
                      {:else}
                        {p.chars} chars{p.pattern_ok === false ? ', misses its pattern' : ', pattern ok'}
                      {/if}
                    </li>
                  {/each}
                </ul>
              {/if}
              <p>
                Key:
                {#if report.key.failure}
                  {failureText(report.key.failure)}
                {:else if report.key.tag}
                  <span class="mono">{report.key.tag}</span>
                {:else}
                  did not render
                {/if}
                {#if report.key_use}
                  · {keyUseText(report.key_use)}
                {/if}
              </p>
              <ul class="checks">
                {#each report.checks as c (c.id)}
                  <li class:failed={c.state === 'refused'} data-testid="trigger-check">
                    <span class="mono">{c.id}</span> {REFUSAL_WORD[c.state]}
                    {#if c.state === 'refused'}({c.code}){/if}
                  </li>
                {/each}
              </ul>
              {#if testPassed}
                <p class="ok-line" data-testid="trigger-test-passed">
                  passes the checks before start; checked at start: {report.checked_at_start.join(', ')}
                </p>
              {/if}
            </div>
          {/if}
        </div>

        <TriggerJournal journal={detail?.journal ?? []} />
      </section>
    {:else if selected}
      <section class="panel" data-testid="trigger-detail">
        {#if detailError}
          <p class="error">{detailError}</p>
        {:else}
          <p class="muted">Loading {selected}.</p>
        {/if}
      </section>
    {/if}
  </div>
</PageScroll>

<!-- The approval dialog: the exact need the server computed, posted back
     unchanged. The server compares it again under the rule's locks and
     refuses anything that changed in between. -->
<AlertDialog.Root bind:open={approveOpen}>
  <AlertDialog.Content data-testid="trigger-approve-dialog">
    {#if need}
      <AlertDialog.Header>
        <AlertDialog.Title>Approve rule {need.rule}</AlertDialog.Title>
        <AlertDialog.Description>
          Approving rule <span class="mono">{need.rule}</span> lets it start {need.playbook}
          {need.version} with nobody at the keyboard.
        </AlertDialog.Description>
      </AlertDialog.Header>
      <div class="need">
        <div class="need-row"><span>rule digest</span><span class="mono">{need.rule_digest}</span></div>
        <div class="need-row"><span>playbook</span><span class="mono">{need.playbook} {need.version}</span></div>
        <div class="need-row"><span>playbook digest</span><span class="mono">{need.playbook_digest}</span></div>
        <div class="need-row"><span>permit digest</span><span class="mono">{need.permit_digest}</span></div>
        <div class="need-row"><span>launch policy</span><span>{need.launch_policy}</span></div>
        {#if need.launch_sources.length}
          <div class="need-row"><span>policy sources</span><span>{need.launch_sources.join(', ')}</span></div>
        {/if}
        {#if need.billing.length}
          <div class="need-row"><span>billing</span><span class="mono">{need.billing.join('; ')}</span></div>
        {/if}
        {#if need.irreversible_sources?.length}
          <div class="need-row">
            <span>irreversible</span><span>{need.irreversible_sources.join(', ')}</span>
          </div>
        {/if}
        {#if need.consent_nonce}
          <div class="need-row"><span>consent nonce</span><span class="mono">{need.consent_nonce}</span></div>
        {/if}
        <div class="need-row"><span>binding</span><span class="mono">{need.binding}</span></div>
      </div>
      <p class="note">
        The approval covers this exact binding. Any change of the rule, the playbook, the launch
        policy or the billing decision takes it away: the rule then starts nothing until a person
        approves it again.
      </p>
      <AlertDialog.Footer>
        <AlertDialog.Cancel data-testid="trigger-approve-cancel">Cancel</AlertDialog.Cancel>
        <AlertDialog.Action
          data-testid="trigger-approve-ok"
          disabled={busy !== null}
          onclick={() => void approve()}
        >
          Approve
        </AlertDialog.Action>
      </AlertDialog.Footer>
    {/if}
  </AlertDialog.Content>
</AlertDialog.Root>

<!-- Removing takes the entry, its state and its journal with it; a rule of
     this name saved later is a new rule with a new incarnation. -->
<AlertDialog.Root bind:open={removeOpen}>
  <AlertDialog.Content data-testid="trigger-remove-dialog">
    <AlertDialog.Header>
      <AlertDialog.Title>Remove rule {selected}</AlertDialog.Title>
      <AlertDialog.Description>
        Removes the rule, its position and its journal. A rule of the same name saved later starts
        nothing until it is approved again.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel data-testid="trigger-remove-cancel">Cancel</AlertDialog.Cancel>
      <AlertDialog.Action
        data-testid="trigger-remove-ok"
        disabled={busy !== null}
        onclick={() => void remove()}
      >
        Remove
      </AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
    width: 100%;
    max-width: 56rem;
    margin: 0 auto;
    padding: 1.5rem 1rem;
    font-size: 0.875rem;
  }
  .page-title {
    font-size: 0.875rem;
    font-weight: 600;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .error {
    color: var(--destructive);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }
  .intro {
    margin: 0;
  }
  .note {
    margin: 0;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--warning);
    color: var(--c-amber);
    font-size: 0.75rem;
  }
  .warn-line {
    color: var(--c-amber);
  }
  .ok-line {
    color: var(--c-green);
  }
  h2 {
    margin: 0;
    font-size: 0.9375rem;
    font-weight: 600;
  }
  h3 {
    margin: 0;
    font-size: 0.8125rem;
    font-weight: 600;
    color: var(--muted-foreground);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg, 8px);
    background: var(--card, var(--background));
  }
  .row.open {
    border-color: var(--c-line-2, var(--border));
  }
  .row-main {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.25rem 1rem;
    padding: 0;
    border: 0;
    background: none;
    color: var(--foreground);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .row-main .name {
    font-weight: 600;
  }
  .badges {
    display: flex;
    flex-wrap: wrap;
    gap: 0.375rem;
  }
  .badge {
    padding: 0 0.5rem;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 0.75rem;
    line-height: 1.25rem;
    color: var(--muted-foreground);
  }
  .badge.on {
    color: var(--foreground);
  }
  .badge.ok {
    border-color: var(--c-green);
    color: var(--c-green);
  }
  .badge.warn {
    border-color: var(--c-amber);
    color: var(--c-amber);
  }

  .panel {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg, 8px);
    background: var(--card, var(--background));
  }
  .panel-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.25rem 1rem;
  }
  .issues {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .issue {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .issue.warning {
    color: var(--c-amber);
  }
  .editor {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .editor-box {
    height: 20rem;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--radius-md, 6px);
    background: var(--background);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }
  .test {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .test-row {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 0.75rem;
  }
  .seq {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.75rem;
    color: var(--muted-foreground);
  }
  .report {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-md, 6px);
  }
  .report p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .conditions,
  .report-list,
  .checks {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }
  .conditions li.failed,
  .checks li.failed {
    color: var(--c-red);
  }
  .need {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-md, 6px);
    font-size: 0.75rem;
  }
  .need-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 0.75rem;
    justify-content: space-between;
  }
  .need-row > span:first-child {
    color: var(--muted-foreground);
    flex-shrink: 0;
  }
  .need-row > span:last-child {
    text-align: right;
    overflow-wrap: anywhere;
  }
</style>
