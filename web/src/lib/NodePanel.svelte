<script lang="ts">
  import { untrack } from 'svelte'
  import type { PlaybookNode } from './types'
  import { profileToField, fieldToProfile } from './profileref'

  let {
    node,
    onChange,
    onDelete,
    revision = 0,
  }: {
    node: PlaybookNode
    onChange: (patch: Record<string, unknown>) => void
    onDelete: () => void
    revision?: number
  } = $props()

  const kind = $derived(node.type)

  // Available profiles from /api/profiles (for the node executor selector).
  // Loaded once on mount; a network error doesn't break the panel - the
  // selector simply keeps a single option plus free-form input via datalist.
  let profiles = $state<{ name: string; scope: string; trusted: boolean }[]>([])
  $effect(() => {
    let cancelled = false
    fetch('/api/profiles')
      .then((r) => (r.ok ? r.json() : { profiles: [] }))
      .then((j) => {
        if (!cancelled && Array.isArray(j.profiles)) profiles = j.profiles
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  })

  // Local field state. Re-synced only when the node changes (id) or on a
  // version reload (revision), not on every field change - otherwise the
  // round trip through yaml would roll back typed text. The patch is emitted
  // on oninput, applied via wfedit on the page side.
  let f = $state<Record<string, string>>({})

  $effect(() => {
    void node.id
    void revision
    untrack(() => {
      const n = node
      f = {
        title: str(n.title),
        prompt: str(n.prompt),
        profile: profileToField(n.profile),
        runner: str(n.runner),
        script: str(n.script),
        outcome: str(n.outcome),
        max_retries: num(n.max_retries),
        max_loops: num(n.max_loops),
        timeout_seconds: num(n.timeout_seconds),
        isolation: str(n.isolation),
        success_check: str(n.success_check),
      }
    })
  })

  function str(v: unknown): string {
    return typeof v === 'string' ? v : v == null ? '' : String(v)
  }
  function num(v: unknown): string {
    return typeof v === 'number' ? String(v) : ''
  }

  // The profile ref is encoded/decoded via profileref (see that module):
  // round-trip typed ref, without collisions between same-named project/global.
  function setProfile(raw: string) {
    f.profile = raw
    onChange({ profile: fieldToProfile(raw) })
  }

  function setStr(key: string, raw: string) {
    f[key] = raw
    onChange(raw === '' ? { [key]: undefined } : { [key]: raw })
  }
  function setNum(key: string, raw: string) {
    f[key] = raw
    if (raw === '') {
      onChange({ [key]: undefined })
      return
    }
    const n = Number(raw)
    onChange(Number.isNaN(n) ? { [key]: undefined } : { [key]: n })
  }
</script>

<div class="panel">
  <div class="panel-head">
    <strong>{node.id}</strong>
    <span class="kind">{kind}</span>
    <button class="btn-del" onclick={onDelete} title="Delete node">delete</button>
  </div>

  <label class="field">
    <span>title</span>
    <input type="text" value={f.title} oninput={(e) => setStr('title', e.currentTarget.value)} />
  </label>

  {#if kind === 'agent_task'}
    <label class="field">
      <span>prompt</span>
      <textarea rows="4" value={f.prompt} oninput={(e) => setStr('prompt', e.currentTarget.value)}></textarea>
    </label>
    <label class="field">
      <span>profile</span>
      <input
        type="text"
        list="apb-profile-options"
        placeholder="name (scope auto) or scope/name"
        value={f.profile}
        oninput={(e) => setProfile(e.currentTarget.value)}
      />
      <datalist id="apb-profile-options">
        {#each profiles as p (p.scope + '/' + p.name)}
          <option value={`${p.scope}/${p.name}`}>{p.trusted ? '' : '(untrusted) '}{p.scope}/{p.name}</option>
        {/each}
      </datalist>
    </label>
    <label class="field">
      <span>max_retries</span>
      <input type="number" value={f.max_retries} oninput={(e) => setNum('max_retries', e.currentTarget.value)} />
    </label>
    <label class="field">
      <span>timeout_seconds</span>
      <input type="number" value={f.timeout_seconds} oninput={(e) => setNum('timeout_seconds', e.currentTarget.value)} />
    </label>
    <label class="field">
      <span>isolation</span>
      <select value={f.isolation} onchange={(e) => setStr('isolation', e.currentTarget.value)}>
        <option value="">(default: none)</option>
        <option value="none">none</option>
        <option value="best_effort">best_effort</option>
        <option value="full">full</option>
      </select>
    </label>
    <label class="field">
      <span>success_check</span>
      <input type="text" value={f.success_check} oninput={(e) => setStr('success_check', e.currentTarget.value)} />
    </label>
  {:else if kind === 'script'}
    <label class="field">
      <span>runner</span>
      <input type="text" value={f.runner} oninput={(e) => setStr('runner', e.currentTarget.value)} />
    </label>
    <label class="field">
      <span>script</span>
      <input type="text" value={f.script} oninput={(e) => setStr('script', e.currentTarget.value)} />
    </label>
    <label class="field">
      <span>timeout_seconds</span>
      <input type="number" value={f.timeout_seconds} oninput={(e) => setNum('timeout_seconds', e.currentTarget.value)} />
    </label>
  {:else if kind === 'condition'}
    <label class="field">
      <span>max_loops</span>
      <input type="number" value={f.max_loops} oninput={(e) => setNum('max_loops', e.currentTarget.value)} />
    </label>
  {:else if kind === 'finish'}
    <label class="field">
      <span>outcome</span>
      <input type="text" value={f.outcome} oninput={(e) => setStr('outcome', e.currentTarget.value)} />
    </label>
  {/if}
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 12px;
  }
  .panel-head {
    display: flex;
    align-items: center;
    gap: 8px;
    border-bottom: 1px solid var(--border);
    padding-bottom: 6px;
  }
  .panel-head .kind {
    color: var(--muted);
    font-size: 11px;
  }
  .panel-head .btn-del {
    margin-left: auto;
    font: inherit;
    font-size: 11px;
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
  }
  .panel-head .btn-del:hover { border-color: var(--err); color: var(--err); }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field > span {
    color: var(--muted);
    font-size: 11px;
  }
  .field input, .field textarea {
    font: inherit;
    font-size: 12px;
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 3px 6px;
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
  }
  .field input:focus, .field textarea:focus {
    outline: none;
    border-color: var(--accent);
  }
</style>
