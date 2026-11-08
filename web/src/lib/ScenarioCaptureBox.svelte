<script lang="ts">
  import { ApiError, jsonHeaders, qs, requestJson } from './api/http'
  import { refusalText } from './api'
  import { Button } from '$lib/components/ui/button'

  // The "Capture as scenario" box of a finished run's page. It asks the
  // server for a preview of what the run leaves as a scenario file (the
  // YAML, the warnings, how many texts the scrubber changed, and the digest
  // that binds the save to what was shown) and saves that file once the
  // person has read the preview. A refusal carries every gap that makes the
  // run uncapturable, listed so a person can read them all.
  let { id, workspace = '' }: { id: string; workspace?: string } = $props()

  // The answer shape of POST /api/runs/{id}/scenario-capture. The generated
  // contract does not carry CapturePreview yet, so the box declares its own
  // minimal interface here.
  interface CapturePreview {
    run_id: string
    playbook: string
    version: string
    name: string
    yaml: string
    digest: string
    redacted: number
    warnings: string[]
    saved?: string
  }

  // Scenario names follow the profile grammar: lowercase letters, digits and
  // dashes, starting with a letter or digit, at most 64 characters.
  const NAME_MAX = 64
  const nameOk = (raw: string) => raw.length <= NAME_MAX && /^[a-z0-9][a-z0-9-]*$/.test(raw)

  let expanded = $state(false)
  let name = $state('')
  let preview = $state<CapturePreview | null>(null)
  let gaps = $state<string[] | null>(null)
  let error = $state<string | null>(null)
  let busy = $state<'preview' | 'save' | null>(null)

  // An inline problem only once there is a value that breaks the grammar; an
  // empty field says nothing before the first keystroke.
  const nameProblem = $derived.by(() => {
    if (name.length === 0 || nameOk(name)) return null
    if (name.length > NAME_MAX) return `At most ${NAME_MAX} characters, this one is ${name.length}.`
    return 'Use lowercase letters, digits and dashes, starting with a letter or digit.'
  })
  const canPreview = $derived(nameOk(name) && busy === null)
  const canSave = $derived.by(() => {
    if (canPreview === false || preview === null) return false
    // A saved capture is done: saving the same name again would be refused.
    return preview.saved === undefined
  })

  function setName(raw: string) {
    name = raw
    // The preview answers the name it was asked for, and its digest covers
    // that capture: a changed name starts the box over.
    preview = null
    gaps = null
    error = null
  }

  function post(body: { name: string; nodes: string[]; save: boolean; digest?: string }) {
    const url = `/api/runs/${encodeURIComponent(id)}/scenario-capture${qs({ workspace })}`
    return requestJson<CapturePreview>(url, {
      method: 'POST',
      headers: jsonHeaders,
      body: JSON.stringify(body),
    })
  }

  // A not_capturable refusal is one sentence: the marker word, then one
  // "code (node): detail" clause per gap, separated by semicolons. Split it
  // so the box can list every gap, not only the first.
  function gapsOf(e: unknown): string[] | null {
    if (e instanceof ApiError && e.status === 409) {
      const text = refusalText(e)
      if (text.startsWith('not_capturable:')) {
        return text
          .slice('not_capturable:'.length)
          .split(';')
          .map((g) => g.trim())
          .filter((g) => g.length > 0)
      }
    }
    return null
  }

  // Bumped per request: an answer that comes back after the person started
  // another one is dropped.
  let token = 0

  async function askPreview() {
    const t = ++token
    busy = 'preview'
    gaps = null
    error = null
    try {
      const r = await post({ name, nodes: [], save: false })
      if (t === token) preview = r
    } catch (e) {
      if (t === token) {
        preview = null
        const found = gapsOf(e)
        if (found === null) error = refusalText(e)
        else gaps = found
      }
    } finally {
      if (t === token) busy = null
    }
  }

  async function save() {
    if (preview === null) return
    const t = ++token
    busy = 'save'
    gaps = null
    error = null
    try {
      // The digest is the preview's: the server recomputes the capture and
      // saves only when it is the one the person read.
      const r = await post({ name, nodes: [], save: true, digest: preview.digest })
      if (t === token) preview = r
    } catch (e) {
      if (t === token) {
        const found = gapsOf(e)
        if (found === null) error = refusalText(e)
        else gaps = found
      }
    } finally {
      if (t === token) busy = null
    }
  }
</script>

<section class="box capture" data-testid="scenario-capture">
  <span class="box-cap">capture as scenario</span>
  {#if expanded === false}
    <div class="row">
      <p class="muted">
        Turn this finished run into a scenario for <span class="mono">apb eval</span>: the capture asserts what the
        run did, and you edit the expectations to what the playbook should do.
      </p>
      <Button variant="outline" size="xs" onclick={() => (expanded = true)} data-testid="capture-open">
        Capture as scenario
      </Button>
    </div>
  {:else}
    <div class="form">
      <label class="field">
        <span class="muted">Scenario name</span>
        <input
          value={name}
          oninput={(e) => setName(e.currentTarget.value)}
          placeholder="green-path"
          spellcheck="false"
          data-testid="capture-name"
        />
      </label>
      {#if nameProblem}<p class="problem" data-testid="capture-name-problem">{nameProblem}</p>{/if}
      <div class="row buttons">
        <Button variant="outline" size="xs" onclick={() => (expanded = false)} data-testid="capture-close">
          Close
        </Button>
        <Button
          variant="outline"
          size="xs"
          disabled={canPreview === false}
          onclick={askPreview}
          data-testid="capture-preview"
        >
          {busy === 'preview' ? 'Capturing' : 'Preview'}
        </Button>
        <Button size="xs" disabled={canSave === false} onclick={save} data-testid="capture-save">
          {busy === 'save' ? 'Saving' : 'Save'}
        </Button>
      </div>
      {#if gaps}
        <div class="gaps" data-testid="capture-gaps">
          <p>This run cannot be captured:</p>
          <ul>
            {#each gaps as g, i (i)}
              <li data-testid="capture-gap">{g}</li>
            {/each}
          </ul>
        </div>
      {/if}
      {#if error}<p class="error" role="status" data-testid="capture-error">{error}</p>{/if}
      {#if preview}
        <div class="result" data-testid="capture-result">
          <p class="meta">
            <span class="muted mono">{preview.playbook}@{preview.version}</span>
            <span class="muted" data-testid="capture-redacted">
              {preview.redacted} {preview.redacted === 1 ? 'text' : 'texts'} redacted
            </span>
            <span class="muted mono" data-testid="capture-digest">digest {preview.digest}</span>
          </p>
          {#if preview.warnings.length > 0}
            <ul class="warnings" data-testid="capture-warnings">
              {#each preview.warnings as w, i (i)}
                <li>{w}</li>
              {/each}
            </ul>
          {/if}
          <pre data-testid="capture-yaml">{preview.yaml}</pre>
          {#if preview.saved}
            <p class="saved" data-testid="capture-saved">Saved to <span class="mono">{preview.saved}</span></p>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</section>

<style>
  .capture {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 1rem 1rem 0.75rem;
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }
  p,
  ul,
  pre {
    margin: 0;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .row p {
    flex: 1 1 14rem;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  input {
    width: 100%;
    min-width: 0;
    max-width: 24rem;
    height: 2rem;
    padding: 0 0.5rem;
    border: 1px solid var(--input, var(--border));
    border-radius: var(--radius-md, 0.375rem);
    background: transparent;
    color: var(--foreground);
    font: inherit;
  }
  input:focus-visible,
  pre:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .buttons {
    justify-content: flex-end;
    align-items: center;
  }
  .problem {
    color: var(--warning);
  }
  .gaps {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.5rem;
    border: 1px solid var(--warning);
  }
  .gaps ul {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    padding-left: 1.1rem;
  }
  .error {
    color: var(--destructive);
  }
  .result {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding-top: 0.375rem;
    border-top: 1px solid var(--border);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.125rem 0.75rem;
  }
  .warnings {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    padding-left: 1.1rem;
    color: var(--warning);
  }
  pre {
    max-height: 24rem;
    overflow: auto;
    padding: 0.5rem;
    border: 1px solid var(--border);
    background: var(--c-panel-2, transparent);
    color: var(--foreground);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.6875rem;
    line-height: 1.5;
    white-space: pre-wrap;
  }
  .saved {
    color: var(--foreground);
  }
  .muted {
    color: var(--muted-foreground);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
</style>
