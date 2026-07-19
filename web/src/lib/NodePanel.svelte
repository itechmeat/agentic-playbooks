<script lang="ts">
  import { untrack } from 'svelte'
  import type { PlaybookNode } from './types'
  import { profileToField, fieldToProfile } from './profileref'
  import { playbookRefToField, fieldToPlaybookRef } from './playbookref'
  import { parseBinding, serializeBinding, toggleListEntry, type ConnectorBinding } from './connectorbinding'
  import { trustBadge, type ConnectorCard, type ConnectorDetail } from './connectors'
  import { fetchInputDraft, saveInputDraft, fetchPlaybooks, fetchConnectors, fetchConnector } from './api'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'
  import { Textarea } from '$lib/components/ui/textarea'
  import { Badge } from '$lib/components/ui/badge'
  import * as Field from '$lib/components/ui/field'
  import * as Select from '$lib/components/ui/select'
  import Combobox from '$lib/components/Combobox.svelte'
  import Trash2 from '@lucide/svelte/icons/trash-2'

  let {
    id,
    node,
    onChange,
    onDelete,
    revision = 0,
    workspace = '',
  }: {
    id: string
    node: PlaybookNode
    onChange: (patch: Record<string, unknown>) => void
    onDelete: () => void
    revision?: number
    workspace?: string
  } = $props()

  const kind = $derived(node.type)

  const ISOLATION = [
    { value: '', label: '(default: none)' },
    { value: 'none', label: 'none' },
    { value: 'best_effort', label: 'best_effort' },
    { value: 'full', label: 'full' },
  ]

  // Available profiles from /api/profiles (for the node executor selector).
  let profiles = $state<{ name: string; scope: string; trusted: boolean }[]>([])
  $effect(() => {
    let cancelled = false
    const url = workspace
      ? `/api/profiles?workspace=${encodeURIComponent(workspace)}`
      : '/api/profiles'
    fetch(url)
      .then((r) => (r.ok ? r.json() : { profiles: [] }))
      .then((j) => {
        if (!cancelled && Array.isArray(j.profiles)) profiles = j.profiles
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  })

  // Available playbooks from /api/playbooks (for the playbook-node reference
  // selector), filtered to this node's own workspace (review R1-I8): the
  // aggregated endpoint spans every registered project, and an unfiltered id
  // can silently resolve to another project's playbook of the same id at run
  // time. The web app has no separate listing for the global playbook store
  // yet (apb_core::store resolves `scope: global` from the machine config
  // dir, not from any project's registry) - "current workspace" is the full
  // set of ids this node can actually resolve today.
  let playbookOptions = $state<{ id: string; project: string }[]>([])
  $effect(() => {
    void workspace
    let cancelled = false
    fetchPlaybooks()
      .then((list) => {
        if (cancelled) return
        const seen = new Set<string>()
        const opts: { id: string; project: string }[] = []
        for (const p of list) {
          if (p.workspace_id !== workspace || seen.has(p.id)) continue
          seen.add(p.id)
          opts.push({ id: p.id, project: p.project })
        }
        playbookOptions = opts
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  })

  // Installed connectors (design doc section 9's node-form bullet), for the
  // add-connector combobox and the untrusted-badge lookup.
  let connectorCards = $state<ConnectorCard[]>([])
  $effect(() => {
    let cancelled = false
    fetchConnectors(workspace)
      .then((list) => {
        if (!cancelled) connectorCards = list
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  })

  // Per-connector detail (account names, function names/read_only flags),
  // fetched lazily by name and cached across nodes/renders. `requested`
  // guards against re-fetching the same name on every reactive pass - it is
  // a plain Set (not $state), it only gates network calls, it never drives
  // rendering.
  let connectorDetails = $state<Record<string, ConnectorDetail>>({})
  const requestedDetails = new Set<string>()
  function ensureDetail(name: string) {
    if (!name || requestedDetails.has(name)) return
    requestedDetails.add(name)
    fetchConnector(name, workspace)
      .then((d) => {
        connectorDetails = { ...connectorDetails, [name]: d }
      })
      .catch(() => {
        requestedDetails.delete(name) // allow a retry on the next add/select
      })
  }

  // Local field state, re-synced only when the node (id) or version (revision)
  // changes - not on every keystroke, else the yaml round-trip rolls back text.
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
        playbook: playbookRefToField(n.playbook),
        instruction: str(n.instruction),
      }
    })
  })

  // Connector bindings (design doc section 5/9): a structural field, so it
  // gets its own state array rather than living in the plain-string `f`
  // record, mirroring how `profile`/`playbook` get dedicated parse/serialize
  // helpers instead of raw string fields.
  let connBindings = $state<ConnectorBinding[]>([])
  $effect(() => {
    void node.id
    void revision
    untrack(() => {
      const raw = node.connectors
      connBindings = Array.isArray(raw) ? raw.map(parseBinding) : []
      for (const b of connBindings) ensureDetail(b.name)
    })
  })

  function syncConnectors(next: ConnectorBinding[]) {
    connBindings = next
    onChange({ connectors: next.length ? next.map(serializeBinding) : undefined })
  }
  function addConnector(name: string) {
    connectorPickerValue = ''
    if (!name || connBindings.some((b) => b.name === name)) return
    ensureDetail(name)
    syncConnectors([...connBindings, { name, functions: 'all' }])
  }
  function removeConnector(name: string) {
    syncConnectors(connBindings.filter((b) => b.name !== name))
  }
  function updateBinding(name: string, patch: Partial<ConnectorBinding>) {
    syncConnectors(connBindings.map((b) => (b.name === name ? { ...b, ...patch } : b)))
  }

  function accountNames(name: string): string[] {
    return (connectorDetails[name]?.accounts ?? []).map((a) => a.name)
  }
  function functionNames(name: string): string[] {
    return (connectorDetails[name]?.functions ?? []).map((fn) => fn.name)
  }
  function readOnlyFunctionNames(name: string): string[] {
    return (connectorDetails[name]?.functions ?? []).filter((fn) => fn.readOnly).map((fn) => fn.name)
  }
  function isAccountChecked(b: ConnectorBinding, account: string): boolean {
    return b.accounts === undefined || b.accounts.includes(account)
  }
  function toggleAccount(b: ConnectorBinding, account: string) {
    const next = toggleListEntry(b.accounts, account, accountNames(b.name))
    updateBinding(b.name, { accounts: next })
  }
  // The list a function-checkbox toggle starts from: every function while
  // the grant is 'all', the connector's read-only set while it is
  // 'read_only', or the explicit list itself. Any toggle from here produces
  // a plain list (or collapses back to undefined), never 'read_only' -
  // manually touching a checkbox always exits the read_only preset.
  function currentFunctionsList(b: ConnectorBinding): string[] | undefined {
    if (b.functions === undefined || b.functions === 'all') return undefined
    if (b.functions === 'read_only') return readOnlyFunctionNames(b.name)
    return b.functions
  }
  function isFunctionChecked(b: ConnectorBinding, fn: string): boolean {
    if (b.functions === undefined || b.functions === 'all') return true
    if (b.functions === 'read_only') return readOnlyFunctionNames(b.name).includes(fn)
    return b.functions.includes(fn)
  }
  function toggleFunction(b: ConnectorBinding, fn: string) {
    const next = toggleListEntry(currentFunctionsList(b), fn, functionNames(b.name))
    updateBinding(b.name, { functions: next })
  }
  function setReadOnlyPreset(b: ConnectorBinding) {
    updateBinding(b.name, { functions: 'read_only' })
  }
  function setMaxCalls(name: string, raw: string) {
    if (raw === '') {
      updateBinding(name, { maxCalls: undefined })
      return
    }
    const n = Number(raw)
    // 0/negative/non-integer max_calls is invalid (validator V26) - ignore
    // rather than write a broken value; the input already carries min="1".
    if (!Number.isInteger(n) || n < 1) return
    updateBinding(name, { maxCalls: n })
  }
  function connectorCard(name: string): ConnectorCard | undefined {
    return connectorCards.find((c) => c.name === name)
  }

  let connectorPickerValue = $state('')
  const connectorOptions = $derived(
    connectorCards
      .filter((c) => !connBindings.some((b) => b.name === c.name))
      .map((c) => ({ value: c.name, label: c.displayName || c.name })),
  )
  $effect(() => {
    const v = connectorPickerValue
    if (v) addConnector(v)
  })

  const badgeClass = {
    ok: 'border-success/30 bg-success/15 text-success',
    warn: 'border-warning/30 bg-warning/15 text-warning',
    danger: 'border-destructive/30 bg-destructive/15 text-destructive',
    muted: '',
  } as const

  // Start-node "input prompt" run draft: a per-run seed the operator can edit
  // before running, stored server-side outside the playbook YAML/version.
  const DRAFT_AUTOSAVE_DEBOUNCE_MS = 500
  let draft = $state('')
  // Quiet inline status for the last load/save attempt (review I9 frontend
  // part): errors used to be swallowed by `.catch(() => {})`, so a failed
  // autosave looked identical to a successful one. Cleared on the next
  // successful save/load; null renders nothing.
  let draftError = $state<string | null>(null)
  let draftTimer: ReturnType<typeof setTimeout> | undefined
  $effect(() => {
    void node.id
    if (node.type !== 'start') return
    let cancelled = false
    fetchInputDraft(id, workspace)
      .then((r) => {
        if (cancelled) return
        draft = r.instruction ?? ''
        draftError = null
      })
      .catch(() => {
        if (!cancelled) draftError = 'draft failed to load'
      })
    return () => {
      cancelled = true
      // Cancel any pending debounced save from the previous node/props before
      // re-running for a new node (or on destroy) - otherwise a stale timer
      // fires after teardown or writes onto the wrong playbook (id/workspace
      // have already moved on since PlaybookEdit is never remounted).
      clearTimeout(draftTimer)
      draftTimer = undefined
    }
  })
  function onDraftInput(v: string) {
    draft = v
    clearTimeout(draftTimer)
    // Capture id/workspace at schedule time so even if this timer somehow
    // survived a node switch, it could not write to a different playbook.
    const saveId = id
    const saveWs = workspace
    draftTimer = setTimeout(() => {
      saveInputDraft(saveId, v, saveWs)
        .then(() => {
          draftError = null
        })
        .catch(() => {
          draftError = 'draft not saved'
        })
    }, DRAFT_AUTOSAVE_DEBOUNCE_MS)
  }

  function str(v: unknown): string {
    return typeof v === 'string' ? v : v == null ? '' : String(v)
  }
  function num(v: unknown): string {
    return typeof v === 'number' ? String(v) : ''
  }

  function setProfile(raw: string) {
    f.profile = raw
    onChange({ profile: fieldToProfile(raw) })
  }
  function setPlaybookRef(raw: string) {
    f.playbook = raw
    onChange({ playbook: fieldToPlaybookRef(raw) })
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

  const isolationLabel = $derived(
    ISOLATION.find((o) => o.value === (f.isolation ?? ''))?.label ?? '(default: none)',
  )
</script>

<div class="flex flex-col gap-3 text-sm">
  <div class="flex items-center gap-2 border-b border-border pb-2">
    <strong class="truncate font-mono">{node.id}</strong>
    <Badge variant="secondary" class="text-[10px]">{kind}</Badge>
    <Button
      variant="ghost"
      size="icon"
      class="ml-auto size-7 text-muted-foreground hover:text-destructive"
      title="Delete node"
      onclick={onDelete}
    >
      <Trash2 />
    </Button>
  </div>

  <datalist id="apb-profile-options">
    {#each profiles as p (p.scope + '/' + p.name)}
      <option value={`${p.scope}/${p.name}`}>
        {p.trusted ? '' : '(untrusted) '}{p.scope}/{p.name}
      </option>
    {/each}
  </datalist>

  <datalist id="apb-playbook-options">
    {#each playbookOptions as p (p.id)}
      <option value={p.id}>{p.id}{p.project ? ` (${p.project})` : ''}</option>
    {/each}
  </datalist>

  <Field.FieldGroup class="gap-3">
    <Field.Field>
      <Field.FieldLabel for="np-title">title</Field.FieldLabel>
      <Input id="np-title" value={f.title} oninput={(e) => setStr('title', e.currentTarget.value)} />
    </Field.Field>

    {#if kind === 'start'}
      <Field.Field>
        <Field.FieldLabel for="np-input">input prompt (run draft, not versioned)</Field.FieldLabel>
        <Textarea
          id="np-input"
          rows={4}
          value={draft}
          oninput={(e) => onDraftInput(e.currentTarget.value)}
        />
        {#if draftError}
          <p class="text-xs text-muted-foreground">{draftError}</p>
        {/if}
      </Field.Field>
    {/if}

    {#if kind === 'agent_task'}
      <Field.Field>
        <Field.FieldLabel for="np-prompt">prompt</Field.FieldLabel>
        <Textarea
          id="np-prompt"
          rows={4}
          value={f.prompt}
          oninput={(e) => setStr('prompt', e.currentTarget.value)}
        />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-profile">profile</Field.FieldLabel>
        <Input
          id="np-profile"
          list="apb-profile-options"
          placeholder="name (scope auto) or scope/name"
          value={f.profile}
          oninput={(e) => setProfile(e.currentTarget.value)}
        />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-retries">max_retries</Field.FieldLabel>
        <Input
          id="np-retries"
          type="number"
          value={f.max_retries}
          oninput={(e) => setNum('max_retries', e.currentTarget.value)}
        />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-timeout">timeout_seconds</Field.FieldLabel>
        <Input
          id="np-timeout"
          type="number"
          value={f.timeout_seconds}
          oninput={(e) => setNum('timeout_seconds', e.currentTarget.value)}
        />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel>isolation</Field.FieldLabel>
        <Select.Root
          type="single"
          value={f.isolation}
          onValueChange={(v) => setStr('isolation', v ?? '')}
        >
          <Select.Trigger class="w-full">{isolationLabel}</Select.Trigger>
          <Select.Content>
            <Select.Group>
              {#each ISOLATION as o (o.value)}
                <Select.Item value={o.value} label={o.label}>{o.label}</Select.Item>
              {/each}
            </Select.Group>
          </Select.Content>
        </Select.Root>
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-success">success_check</Field.FieldLabel>
        <Input
          id="np-success"
          value={f.success_check}
          oninput={(e) => setStr('success_check', e.currentTarget.value)}
        />
      </Field.Field>

      <Field.Field>
        <Field.FieldLabel>connectors</Field.FieldLabel>
        <div class="flex flex-col gap-2">
          {#each connBindings as b (b.name)}
            {@const card = connectorCard(b.name)}
            {@const detail = connectorDetails[b.name]}
            <div class="flex flex-col gap-2 rounded-md border border-border p-2">
              <div class="flex items-center gap-2">
                <span class="truncate font-mono text-xs">{b.name}</span>
                {#if card && card.trust !== 'approved'}
                  {@const badge = trustBadge(card.trust)}
                  <Badge variant="outline" class={badgeClass[badge.tone]}>{badge.label}</Badge>
                {/if}
                <Button
                  variant="ghost"
                  size="icon"
                  class="ml-auto size-6 text-muted-foreground hover:text-destructive"
                  title="Remove connector"
                  onclick={() => removeConnector(b.name)}
                >
                  <Trash2 class="size-3.5" />
                </Button>
              </div>

              {#if detail}
                {#if detail.accounts.length > 0}
                  <div class="flex flex-col gap-1">
                    <span class="text-xs text-muted-foreground">accounts (unchecked: not granted)</span>
                    <div class="flex flex-wrap gap-3">
                      {#each detail.accounts as a (a.name)}
                        <label class="flex items-center gap-1 text-xs">
                          <input
                            type="checkbox"
                            checked={isAccountChecked(b, a.name)}
                            onchange={() => toggleAccount(b, a.name)}
                          />
                          {a.name}
                        </label>
                      {/each}
                    </div>
                  </div>
                {/if}

                {#if detail.functions.length > 0}
                  <div class="flex flex-col gap-1">
                    <div class="flex items-center gap-2">
                      <span class="text-xs text-muted-foreground">functions (unchecked: not granted)</span>
                      <Button
                        variant={b.functions === 'read_only' ? 'secondary' : 'outline'}
                        size="sm"
                        class="h-6 px-2 text-xs"
                        onclick={() => setReadOnlyPreset(b)}
                      >
                        read_only
                      </Button>
                    </div>
                    <div class="flex flex-wrap gap-3">
                      {#each detail.functions as fn (fn.name)}
                        <label class="flex items-center gap-1 text-xs">
                          <input
                            type="checkbox"
                            checked={isFunctionChecked(b, fn.name)}
                            onchange={() => toggleFunction(b, fn.name)}
                          />
                          {fn.name}
                        </label>
                      {/each}
                    </div>
                  </div>
                {/if}
              {/if}

              <div class="flex items-center gap-2">
                <Field.FieldLabel for={`np-conn-max-${b.name}`} class="text-xs">max_calls</Field.FieldLabel>
                <Input
                  id={`np-conn-max-${b.name}`}
                  type="number"
                  min="1"
                  class="h-7 w-24"
                  value={b.maxCalls ?? ''}
                  oninput={(e) => setMaxCalls(b.name, e.currentTarget.value)}
                />
              </div>
            </div>
          {/each}

          <Combobox
            bind:value={connectorPickerValue}
            options={connectorOptions}
            placeholder="Add connector..."
            emptyText="No connectors available"
            allowCustom={false}
          />
        </div>
      </Field.Field>
    {:else if kind === 'script'}
      <Field.Field>
        <Field.FieldLabel for="np-runner">runner</Field.FieldLabel>
        <Input id="np-runner" value={f.runner} oninput={(e) => setStr('runner', e.currentTarget.value)} />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-script">script</Field.FieldLabel>
        <Input id="np-script" value={f.script} oninput={(e) => setStr('script', e.currentTarget.value)} />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-stimeout">timeout_seconds</Field.FieldLabel>
        <Input
          id="np-stimeout"
          type="number"
          value={f.timeout_seconds}
          oninput={(e) => setNum('timeout_seconds', e.currentTarget.value)}
        />
      </Field.Field>
    {:else if kind === 'condition'}
      <Field.Field>
        <Field.FieldLabel for="np-loops">max_loops</Field.FieldLabel>
        <Input
          id="np-loops"
          type="number"
          value={f.max_loops}
          oninput={(e) => setNum('max_loops', e.currentTarget.value)}
        />
      </Field.Field>
    {:else if kind === 'finish'}
      <Field.Field>
        <Field.FieldLabel for="np-outcome">outcome</Field.FieldLabel>
        <Input id="np-outcome" value={f.outcome} oninput={(e) => setStr('outcome', e.currentTarget.value)} />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-finish-prompt">prompt (compose the run answer; optional)</Field.FieldLabel>
        <Textarea
          id="np-finish-prompt"
          rows={4}
          value={f.prompt}
          oninput={(e) => setStr('prompt', e.currentTarget.value)}
        />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-finish-profile">profile</Field.FieldLabel>
        <Input
          id="np-finish-profile"
          list="apb-profile-options"
          placeholder="name (scope auto) or scope/name"
          value={f.profile}
          oninput={(e) => setProfile(e.currentTarget.value)}
        />
      </Field.Field>
    {:else if kind === 'playbook'}
      <Field.Field>
        <Field.FieldLabel for="np-pb-ref">playbook</Field.FieldLabel>
        <Input
          id="np-pb-ref"
          list="apb-playbook-options"
          placeholder={'id (scope auto) or scope/id, e.g. global/child'}
          value={f.playbook}
          oninput={(e) => setPlaybookRef(e.currentTarget.value)}
        />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-pb-instr">instruction (rendered, becomes the child input)</Field.FieldLabel>
        <Textarea
          id="np-pb-instr"
          rows={4}
          value={f.instruction}
          oninput={(e) => setStr('instruction', e.currentTarget.value)}
        />
      </Field.Field>
    {/if}
  </Field.FieldGroup>
</div>
