<script lang="ts">
  import { parse } from 'yaml'
  import {
    fetchAgents,
    fetchModels,
    fetchProfile,
    fetchProjects,
    fetchSkills,
    writeProfile,
    type AgentInfo,
    type ModelRow,
    type AvailableSkill,
  } from '../lib/api'
  import type { Project } from '../lib/types'
  import Topbar from '$lib/components/Topbar.svelte'
  import Combobox from '$lib/components/Combobox.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'
  import { Textarea } from '$lib/components/ui/textarea'
  import { Label } from '$lib/components/ui/label'
  import { Switch } from '$lib/components/ui/switch'
  import { Spinner } from '$lib/components/ui/spinner'
  import * as Field from '$lib/components/ui/field'
  import * as Select from '$lib/components/ui/select'
  import * as RadioGroup from '$lib/components/ui/radio-group'
  import { toast } from 'svelte-sonner'

  let {
    name = '',
    scope = 'project',
    workspace = '',
  }: { name?: string; scope?: string; workspace?: string } = $props()

  const isNew = $derived(!name)

  let projects = $state<Project[]>([])
  let agents = $state<AgentInfo[]>([])
  let modelsTable = $state<ModelRow[]>([])
  let availableSkills = $state<AvailableSkill[]>([])

  const DESC_MAX = 200

  let nameInput = $state(name)
  let scopeInput = $state(scope)
  let workspaceInput = $state(workspace)
  let agent = $state('')
  let model = $state('')
  let description = $state('')
  let skills = $state<string[]>([])
  let soul = $state('')
  let expectedDigest = $state<string | null>(null)
  let saving = $state(false)

  // Monotonic token: an in-flight profile fetch that resolves after a newer
  // load started (fast navigation between profiles) is ignored, so it cannot
  // overwrite the current form.
  let loadToken = 0

  const projectName = $derived(
    projects.find((p) => p.workspace_id === workspaceInput)?.name ?? 'select a project',
  )

  // Only installed agents are offered. Detected agent ids are canonical
  // (e.g. `claude`, not `claude-code`); both resolve to the same CLI, so we
  // standardize on the detected id everywhere.
  const agentOptions = $derived(
    agents.filter((a) => a.installed).map((a) => ({ value: a.agent })),
  )

  // Model suggestions depend on the chosen agent: prefer that agent's detected
  // model list; fall back to the curated table. Legacy `claude-code` probes as
  // `claude`.
  const modelOptions = $derived.by(() => {
    const probe = agent === 'claude-code' ? 'claude' : agent
    const a = agents.find((x) => x.agent === probe)
    const ids = a?.models?.items?.length ? a.models.items : modelsTable.map((m) => m.id)
    return ids.map((id) => ({ value: id }))
  })

  // Skill rows: available skills, plus any selected-but-missing ones surfaced on
  // top so editing never silently drops a referenced skill.
  const skillRows = $derived.by(() => {
    const avail = new Set(availableSkills.map((s) => s.name))
    const missing = skills
      .filter((s) => !avail.has(s))
      .map((s) => ({ name: s, scope: 'missing' }))
    return [...missing, ...availableSkills]
  })

  const hasSkill = (n: string) => skills.includes(n)
  function toggleSkill(n: string) {
    skills = hasSkill(n) ? skills.filter((s) => s !== n) : [...skills, n]
  }

  async function loadMeta() {
    try {
      const [pj, ag, md] = await Promise.all([fetchProjects(), fetchAgents(), fetchModels()])
      projects = pj
      agents = ag
      modelsTable = md.models
      if (isNew && !workspaceInput && projects.length) workspaceInput = projects[0].workspace_id
      // New profile: default to an installed agent (prefer claude) so the
      // default value always matches a selector option.
      if (isNew && !agent) {
        const installed = ag.filter((a) => a.installed)
        agent = installed.find((a) => a.agent === 'claude')?.agent ?? installed[0]?.agent ?? 'claude'
      }
    } catch (e) {
      toast.error('Failed to load form data', { description: String(e) })
    }
  }

  async function loadProfile(token: number) {
    if (isNew) return
    try {
      const d = await fetchProfile(name, scope, workspace)
      if (token !== loadToken) return
      expectedDigest = d.profile_digest
      soul = d.soul_md
      const doc = (parse(d.profile_yaml) ?? {}) as {
        description?: string
        executor?: { agent?: string; model?: string }
        skills?: (string | { name: string })[]
      }
      agent = doc.executor?.agent ?? 'claude-code'
      model = doc.executor?.model ?? ''
      description = doc.description ?? ''
      skills = (doc.skills ?? []).map((s) => (typeof s === 'string' ? s : s.name))
    } catch (e) {
      if (token === loadToken) toast.error('Failed to load profile', { description: String(e) })
    }
  }

  // Reload the profile when the route target changes. Re-sync the identity
  // fields (name/scope/project) from the new props AND clear the editable
  // fields first - otherwise navigating (incl. browser back/forward) between
  // two profile-edit routes reuses this component and shows the previous
  // profile's data (and digest) until the new load resolves.
  $effect(() => {
    nameInput = name
    scopeInput = scope
    if (workspace) workspaceInput = workspace
    const token = ++loadToken
    expectedDigest = null
    soul = ''
    model = ''
    description = ''
    skills = []
    loadProfile(token)
  })
  $effect(() => {
    loadMeta()
  })

  // Available skills depend on scope (project sees project+global; global sees
  // only global) and the chosen project.
  $effect(() => {
    const ws = scopeInput === 'project' ? workspaceInput : ''
    if (scopeInput === 'project' && !ws) {
      availableSkills = []
      return
    }
    let cancelled = false
    fetchSkills(scopeInput, ws)
      .then((s) => {
        if (!cancelled) availableSkills = s
      })
      .catch(() => {
        if (!cancelled) availableSkills = []
      })
    return () => {
      cancelled = true
    }
  })

  async function save() {
    const nm = nameInput.trim()
    if (!nm) return toast.error('Name is required')
    if (!agent.trim()) return toast.error('Agent is required')
    if (!model.trim()) return toast.error('Model is required')
    if (scopeInput === 'project' && !workspaceInput) return toast.error('Select a project')
    // For an update, the current profile digest drives optimistic concurrency.
    // If it never loaded (null), refuse rather than sending no digest and
    // silently overwriting a profile we never read.
    if (!isNew && expectedDigest === null) {
      return toast.error('Profile has not finished loading; try again')
    }
    saving = true
    try {
      const ws = scopeInput === 'project' ? workspaceInput : ''
      await writeProfile(
        {
          name: nm,
          scope: scopeInput,
          agent: agent.trim(),
          model: model.trim(),
          description,
          soul,
          skills,
          soul_requirement: 'any',
          expected_digest: isNew ? undefined : (expectedDigest ?? undefined),
        },
        ws,
      )
      toast.success(`Saved profile "${nm}"`)
      location.hash = '#/profiles'
    } catch (e) {
      toast.error('Save failed', { description: String(e) })
    } finally {
      saving = false
    }
  }
</script>

<Topbar active="profiles">
  {#snippet title()}
    <span class="truncate text-sm font-medium">{isNew ? 'New profile' : name}</span>
  {/snippet}
  {#snippet actions()}
    <Button size="sm" onclick={save} disabled={saving}>
      {#if saving}<Spinner data-icon="inline-start" />{/if}
      {saving ? 'Saving...' : 'Save'}
    </Button>
  {/snippet}
</Topbar>

<div class="min-h-0 flex-1 overflow-auto">
  <div class="mx-auto w-full max-w-2xl px-4 py-6">
    <Field.FieldGroup>
      <Field.Field>
        <Field.FieldLabel for="pf-name">Name</Field.FieldLabel>
        <Field.FieldDescription>Lowercase, digits and hyphens. Immutable after creation.</Field.FieldDescription>
        <Input id="pf-name" bind:value={nameInput} placeholder="profile-name" disabled={!isNew} />
      </Field.Field>

      <Field.Field>
        <Field.FieldLabel for="pf-desc">Description</Field.FieldLabel>
        <Field.FieldDescription>
          Short summary shown in the profile list. {description.length}/{DESC_MAX}
        </Field.FieldDescription>
        <Textarea
          id="pf-desc"
          bind:value={description}
          rows={2}
          maxlength={DESC_MAX}
          class="resize-none"
        />
      </Field.Field>

      <Field.Field>
        <Field.FieldLabel for="pf-soul">SOUL (role prompt)</Field.FieldLabel>
        <Field.FieldDescription>The main field: the role/system prompt for this profile.</Field.FieldDescription>
        <Textarea
          id="pf-soul"
          bind:value={soul}
          rows={12}
          class="font-mono text-sm"
          placeholder="The system prompt that defines this executor's role."
        />
      </Field.Field>

      <Field.Field>
        <Field.FieldLabel>Scope</Field.FieldLabel>
        <RadioGroup.Root bind:value={scopeInput} class="flex flex-row gap-6" disabled={!isNew}>
          <div class="flex items-center gap-2">
            <RadioGroup.Item value="project" id="scope-project" />
            <Label for="scope-project" class="font-normal">project</Label>
          </div>
          <div class="flex items-center gap-2">
            <RadioGroup.Item value="global" id="scope-global" />
            <Label for="scope-global" class="font-normal">global</Label>
          </div>
        </RadioGroup.Root>
      </Field.Field>

      {#if scopeInput === 'project'}
        <Field.Field>
          <Field.FieldLabel>Project</Field.FieldLabel>
          <Select.Root type="single" bind:value={workspaceInput} disabled={!isNew}>
            <Select.Trigger class="w-full">{projectName}</Select.Trigger>
            <Select.Content>
              <Select.Group>
                {#each projects as p (p.workspace_id)}
                  <Select.Item value={p.workspace_id} label={p.name}>{p.name}</Select.Item>
                {/each}
              </Select.Group>
            </Select.Content>
          </Select.Root>
        </Field.Field>
      {/if}

      <Field.Field>
        <Field.FieldLabel for="pf-agent">Agent</Field.FieldLabel>
        <Field.FieldDescription>Pick a detected agent or type a custom name.</Field.FieldDescription>
        <Combobox
          id="pf-agent"
          bind:value={agent}
          options={agentOptions}
          placeholder="claude"
          emptyText="No installed agents"
        />
      </Field.Field>

      <Field.Field>
        <Field.FieldLabel for="pf-model">Model</Field.FieldLabel>
        <Field.FieldDescription>Suggestions depend on the selected agent. Custom values allowed.</Field.FieldDescription>
        <Combobox
          id="pf-model"
          bind:value={model}
          options={modelOptions}
          placeholder="claude-opus-4-8"
          emptyText="No models for this agent"
        />
      </Field.Field>

      <Field.Field>
        <Field.FieldLabel>Skills</Field.FieldLabel>
        <Field.FieldDescription>Toggle the skills this profile grants.</Field.FieldDescription>
        <div class="rounded-md border border-border">
          {#if skillRows.length === 0}
            <p class="px-3 py-6 text-center text-sm text-muted-foreground">
              No skills available in this scope.
            </p>
          {:else}
            <ul class="divide-y divide-border">
              {#each skillRows as s (s.name)}
                <li class="flex items-center gap-3 px-3 py-2">
                  <Switch
                    id={`skill-${s.name}`}
                    checked={hasSkill(s.name)}
                    onCheckedChange={() => toggleSkill(s.name)}
                  />
                  <Label for={`skill-${s.name}`} class="flex-1 cursor-pointer font-normal">
                    {s.name}
                  </Label>
                  <span
                    class="text-xs {s.scope === 'missing'
                      ? 'text-warning'
                      : 'text-muted-foreground'}"
                  >
                    {s.scope}
                  </span>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      </Field.Field>
    </Field.FieldGroup>
  </div>
</div>
