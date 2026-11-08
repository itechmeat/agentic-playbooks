<script lang="ts">
  import type { Tone } from './runverdict'
  import { jsonHeaders, pb, qs, requestJson } from './api/http'
  import { refusalText } from './api'
  import { runsText } from './knownproblems'
  import OutcomeSection from '$lib/components/OutcomeSection.svelte'
  import OutcomeTag from '$lib/components/OutcomeTag.svelte'
  import { Button } from '$lib/components/ui/button'

  // A playbook's corrections ledger: the words people added at review
  // gates, the answers they gave a node's questions, and the results a
  // check rejected, folded per wording across runs. Once a wording recurs
  // it is proposed, with a draft line a person copies into the node's
  // prompt themselves (apb never applies one) and then marks the version
  // that carries the fix; the fold reopens the entry when the correction
  // keeps meeting its old rate on that version. A file this installation
  // did not write is only counted.
  let {
    id,
    workspace = '',
    versions = [],
  }: {
    id: string
    workspace?: string
    /** The playbook's versions, newest first, for the "mark fixed" picker; a text input when empty. */
    versions?: string[]
  } = $props()

  // The answer shapes of GET /api/playbooks/{id}/corrections. The generated
  // contract does not carry the corrections types yet, so the card declares
  // its own minimal interfaces here.
  interface Decided {
    by: string
    at_ms: number
    reason?: string
  }
  interface FixRecord {
    version: string
    base_version: string
    base_runs: number
    base_hits: number
    marked: Decided
  }
  interface Correction {
    id: string
    kind: string
    node: string | null
    gate: string | null
    text: string | null
    runs: number
    via_supervisor: boolean
    status: 'open' | 'proposed' | 'fixed' | 'reopened' | 'dismissed'
    fix?: FixRecord | null
    last_version?: string
  }
  interface Draft {
    node: string
    line: string
  }
  interface FixRate {
    base_runs: number
    base_hits: number
    fix_runs: number
    fix_hits: number
  }
  interface CorrectionView {
    correction: Correction
    draft: Draft | null
    rate: FixRate | null
  }
  interface CorrectionsView {
    playbook: string
    scope: string
    signed: boolean
    unsigned_entries: number
    entries: CorrectionView[]
  }

  type Status = Correction['status']

  let view = $state<CorrectionsView | null>(null)
  let loading = $state(false)
  let error = $state<string | null>(null)
  // The entry whose action is in flight: its own buttons are disabled.
  let busy = $state<string | null>(null)
  // A refused action, shown on the entry it came from.
  let actionError = $state<{ entry: string; message: string } | null>(null)
  // The entry whose "mark fixed" is one click from being sent.
  let confirming = $state<string | null>(null)
  // The version picked per entry for "mark fixed", and the optional reason.
  let fixVersions = $state<Record<string, string>>({})
  let reasons = $state<Record<string, string>>({})
  let copied = $state<string | null>(null)
  let copyFailed = $state<string | null>(null)

  // Bumped per (re)load: a response for an older load (the playbook moved
  // on since) writes nothing.
  let token = 0

  async function load() {
    const t = ++token
    loading = true
    error = null
    try {
      const v = await requestJson<CorrectionsView>(`${pb(id)}/corrections${qs({ workspace })}`, {
        method: 'GET',
      })
      if (t === token) {
        view = v
        loading = false
      }
    } catch (e) {
      if (t === token) {
        error = refusalText(e)
        loading = false
      }
    }
  }

  $effect(() => {
    void id
    void workspace
    view = null
    confirming = null
    void load()
  })

  const kindWord: Record<string, string> = {
    review_note: 'review note',
    node_note: 'node note',
    human_answer: 'question answer',
    rejection: 'rejection',
  }

  function statusWord(s: Status): string {
    return s
  }
  function statusTone(s: Status): Tone {
    switch (s) {
      case 'proposed':
        return 'gap'
      case 'reopened':
        return 'fail'
      case 'fixed':
        return 'pass'
      default:
        return 'neutral'
    }
  }

  // Entries by status: the ones a person should act on first (proposed and
  // reopened, both offering a draft), then the ones seen once so far, the
  // marked fixed, and the dismissed.
  const sections = $derived.by(() => {
    const groups: Record<string, CorrectionView[]> = { active: [], once: [], fixed: [], dismissed: [] }
    for (const e of view?.entries ?? []) {
      const s = e.correction.status
      if (s === 'proposed' || s === 'reopened') groups.active.push(e)
      else if (s === 'open') groups.once.push(e)
      else if (s === 'fixed') groups.fixed.push(e)
      else groups.dismissed.push(e)
    }
    return [
      { key: 'active', title: 'Proposed and reopened', items: groups.active },
      { key: 'once', title: 'Seen once', items: groups.once },
      { key: 'fixed', title: 'Marked fixed', items: groups.fixed },
      { key: 'dismissed', title: 'Dismissed', items: groups.dismissed },
    ].filter((s) => s.items.length > 0)
  })

  const offerDraft = (s: Status) => s === 'proposed' || s === 'reopened'
  const canFix = offerDraft
  const canDismiss = (s: Status) => s !== 'dismissed'
  const canReopen = (s: Status) => s === 'dismissed' || s === 'fixed'

  const versionOf = (entry: string) => (fixVersions[entry] ?? '').trim()
  const reasonOf = (entry: string) => (reasons[entry] ?? '').trim()
  const withReason = (entry: string): Record<string, string> => {
    const reason = reasonOf(entry)
    return reason ? { reason } : {}
  }

  // Both rates of a fixed or reopened entry: how often it still met on the
  // fix version, and what it was before. The versions come from the fix
  // record, which a reopened entry has lost, so the words fall back to
  // counts only.
  function ratesText(e: CorrectionView): string | null {
    const r = e.rate
    if (!r) return null
    const on = e.correction.fix ? ` on ${e.correction.fix.version}` : ''
    return `${r.fix_hits} of ${r.fix_runs} runs${on}; was ${r.base_hits} of ${r.base_runs}`
  }

  function metaText(c: Correction): string {
    const parts = [runsText(c.runs), kindWord[c.kind] ?? c.kind]
    if (c.gate) parts.push(`gate ${c.gate}`)
    if (c.via_supervisor) parts.push('via the supervisor channel')
    return parts.join(' · ')
  }

  const unsignedLine = (n: number) =>
    `The ledger file was not written by this installation (${n} ${n === 1 ? 'entry' : 'entries'} withheld).`

  async function copyDraft(e: CorrectionView) {
    if (!e.draft) return
    const cid = e.correction.id
    copied = null
    copyFailed = null
    try {
      await navigator.clipboard.writeText(e.draft.line)
      copied = cid
    } catch {
      copyFailed = cid
    }
  }

  async function act(e: CorrectionView, action: 'fixed' | 'dismiss' | 'reopen') {
    const cid = e.correction.id
    const body =
      action === 'fixed' ? { version: versionOf(cid), ...withReason(cid) } : { ...withReason(cid) }
    const t = token
    busy = cid
    actionError = null
    try {
      await requestJson(`${pb(id)}/corrections/${encodeURIComponent(cid)}/${action}${qs({ workspace })}`, {
        method: 'POST',
        headers: jsonHeaders,
        body: JSON.stringify(body),
      })
      if (t !== token) return
      confirming = null
      await load()
    } catch (err) {
      if (t !== token) return
      actionError = { entry: cid, message: refusalText(err) }
    } finally {
      if (t === token) busy = null
    }
  }
</script>

<OutcomeSection title="Corrections from earlier runs" testid="corrections-card">
  {#if loading && !view}
    <p class="muted">Loading...</p>
  {:else if error}
    <p class="error" data-testid="corrections-error">{error}</p>
  {:else if view && !view.signed}
    <p class="error" data-testid="corrections-unsigned">{unsignedLine(view.unsigned_entries)}</p>
  {:else if !view || view.entries.length === 0}
    <p class="muted" data-testid="corrections-empty">
      No corrections yet. Runs collect the words people add at review gates, the answers they give a
      node's questions, and the results a check rejected.
    </p>
  {:else}
    {#each sections as s (s.key)}
      <div class="group" data-testid="corrections-group" data-group={s.key}>
        <h4>{s.title} <span class="muted">({s.items.length})</span></h4>
        <ul>
          {#each s.items as e (e.correction.id)}
            {@const c = e.correction}
            {@const rates = ratesText(e)}
            <li data-testid="corrections-entry" data-id={c.id} data-status={c.status}>
              <div class="row">
                <OutcomeTag tone={statusTone(c.status)}>{statusWord(c.status)}</OutcomeTag>
                {#if c.node}<span class="mono node">{c.node}</span>{/if}
              </div>
              {#if c.text}
                <p class="text" data-testid="corrections-text">{c.text}</p>
              {:else}
                <p class="muted" data-testid="corrections-text">text withheld</p>
              {/if}
              <p class="muted meta">{metaText(c)}</p>
              {#if rates}
                <p class="muted" data-testid="corrections-rates">{rates}</p>
              {/if}
              {#if e.draft}
                <div class="draft" data-testid="corrections-draft">
                  <p class="muted">Draft for node <span class="mono">{e.draft.node}</span>:</p>
                  <code class="draft-line">{e.draft.line}</code>
                  <div class="buttons">
                    <Button
                      variant="outline"
                      size="sm"
                      onclick={() => copyDraft(e)}
                      data-testid="corrections-copy"
                      title="Copy the draft line, paste it into the node's prompt yourself"
                    >
                      Copy
                    </Button>
                    {#if copied === c.id}
                      <span class="copied" data-testid="corrections-copied">Copied</span>
                    {:else if copyFailed === c.id}
                      <span class="error" data-testid="corrections-copy-failed">Could not copy; select the line instead</span>
                    {/if}
                  </div>
                  <p class="muted hint">
                    Add this line to the node's prompt the way you usually change a playbook (the
                    editor, apb version tools, or a supervisor patch you promote), then mark below
                    the version that carries the fix.
                  </p>
                </div>
              {/if}

              {#if canFix(c.status)}
                <div class="fix-row">
                  {#if versions.length > 0}
                    <select
                      class="version"
                      data-testid="corrections-version"
                      value={fixVersions[c.id] ?? ''}
                      onchange={(ev) => (fixVersions[c.id] = ev.currentTarget.value)}
                      title="The version that carries the fix"
                    >
                      <option value="" disabled>version</option>
                      {#each versions as v (v)}
                        <option value={v}>{v}</option>
                      {/each}
                    </select>
                  {:else}
                    <input
                      class="version"
                      type="text"
                      data-testid="corrections-version"
                      placeholder="version, e.g. 1.2.0"
                      value={fixVersions[c.id] ?? ''}
                      oninput={(ev) => (fixVersions[c.id] = ev.currentTarget.value)}
                    />
                  {/if}
                  {#if confirming === c.id}
                    <span data-testid="corrections-fixed-question">Mark fixed in {versionOf(c.id)}?</span>
                    <Button
                      size="sm"
                      disabled={busy === c.id || !versionOf(c.id)}
                      onclick={() => act(e, 'fixed')}
                      data-testid="corrections-fixed-confirm"
                    >
                      Mark fixed
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy === c.id}
                      onclick={() => (confirming = null)}
                      data-testid="corrections-fixed-cancel"
                    >
                      Cancel
                    </Button>
                  {:else}
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy === c.id || !versionOf(c.id)}
                      onclick={() => (confirming = c.id)}
                      data-testid="corrections-fixed"
                      title="Say which version carries the fix; the fold reopens the entry if the correction keeps meeting its old rate there"
                    >
                      Mark fixed...
                    </Button>
                  {/if}
                </div>
              {/if}

              {#if canDismiss(c.status) || canReopen(c.status)}
                <div class="fix-row">
                  <input
                    class="reason"
                    type="text"
                    maxlength="300"
                    placeholder="Reason (optional)"
                    data-testid="corrections-reason"
                    value={reasons[c.id] ?? ''}
                    oninput={(ev) => (reasons[c.id] = ev.currentTarget.value)}
                  />
                  {#if canDismiss(c.status)}
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy === c.id}
                      onclick={() => act(e, 'dismiss')}
                      data-testid="corrections-dismiss"
                      title="Never proposed to anyone again"
                    >
                      Dismiss
                    </Button>
                  {/if}
                  {#if canReopen(c.status)}
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy === c.id}
                      onclick={() => act(e, 'reopen')}
                      data-testid="corrections-reopen"
                      title="Back to proposed: the fix did not hold, or it was dismissed by mistake"
                    >
                      Reopen
                    </Button>
                  {/if}
                </div>
              {/if}

              {#if actionError?.entry === c.id}
                <p class="error" data-testid="corrections-action-error">Not done: {actionError.message}</p>
              {/if}
            </li>
          {/each}
        </ul>
      </div>
    {/each}
  {/if}
</OutcomeSection>

<style>
  p {
    margin: 0;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  h4 {
    margin: 0;
    font-size: 0.75rem;
    font-weight: 600;
  }
  ul {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding-bottom: 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  li:last-child {
    border-bottom: 0;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  .node {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .text {
    overflow-wrap: anywhere;
  }
  .draft {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.5rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm, 0.25rem);
  }
  .draft-line {
    padding: 0.25rem 0.375rem;
    border-left: 2px solid var(--warning);
    font-family: var(--font-mono, ui-monospace, monospace);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .hint {
    padding-left: 0.5rem;
    border-left: 2px solid var(--border);
  }
  .copied {
    color: var(--success, var(--muted-foreground));
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  .fix-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  .version {
    width: 9rem;
  }
  .reason {
    flex: 1 1 10rem;
    min-width: 0;
  }
  input,
  select {
    height: 1.75rem;
    padding: 0 0.375rem;
    border: 1px solid var(--input, var(--border));
    border-radius: var(--radius-sm, 0.25rem);
    background: var(--background);
    color: var(--foreground);
    font: inherit;
  }
  select:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .error {
    color: var(--destructive);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
</style>
