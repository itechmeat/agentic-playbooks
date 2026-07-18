<script lang="ts">
  import { untrack } from 'svelte'
  import type { PlaybookNode } from './types'
  import { profileToField, fieldToProfile } from './profileref'
  import { fetchInputDraft, saveInputDraft } from './api'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'
  import { Textarea } from '$lib/components/ui/textarea'
  import { Badge } from '$lib/components/ui/badge'
  import * as Field from '$lib/components/ui/field'
  import * as Select from '$lib/components/ui/select'
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
      }
    })
  })

  // Start-node "input prompt" run draft: a per-run seed the operator can edit
  // before running, stored server-side outside the playbook YAML/version.
  let draft = $state('')
  let draftTimer: ReturnType<typeof setTimeout> | undefined
  $effect(() => {
    void node.id
    if (node.type !== 'start') return
    let cancelled = false
    fetchInputDraft(id, workspace)
      .then((r) => {
        if (!cancelled) draft = r.instruction ?? ''
      })
      .catch(() => {})
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
      saveInputDraft(saveId, v, saveWs).catch(() => {})
    }, 500)
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
        <datalist id="apb-profile-options">
          {#each profiles as p (p.scope + '/' + p.name)}
            <option value={`${p.scope}/${p.name}`}>
              {p.trusted ? '' : '(untrusted) '}{p.scope}/{p.name}
            </option>
          {/each}
        </datalist>
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
    {/if}
  </Field.FieldGroup>
</div>
