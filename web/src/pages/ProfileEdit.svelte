<script lang="ts">
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
  import {
    defaultPrimaryPair,
    disabledModelIds,
    findDuplicatePair,
    firstSelectableModelForAgent,
    modelIdsForAgent,
    nextFallbackPair,
    parseProfileDoc,
    type ExecutorGroup,
  } from '../lib/profileedit'
  import type { Project } from '../lib/types'
  import { untrack } from 'svelte'
  import Plus from '@lucide/svelte/icons/plus'
  import Trash2 from '@lucide/svelte/icons/trash-2'
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
  // The executor chain in walk order: index 0 is the primary executor and is
  // always present; everything after it is a fallback. Order is meaningful and
  // is preserved verbatim on load and on save.
  let groups = $state<ExecutorGroup[]>([{ agent: '', model: '' }])
  let description = $state('')
  let skills = $state<string[]>([])
  let soul = $state('')
  let expectedDigest = $state<string | null>(null)
  let saving = $state(false)
  // Whether the executor metadata (agents + models table) has settled. Tracked
  // explicitly rather than inferred from `agents.length`, so "the /api/agents
  // call has not landed yet" (detection takes seconds on a cold server) stays
  // distinguishable from "detection ran and no agent is installed". Only the
  // first case must disable the executor actions; the second is a real state
  // the guards below are allowed to report.
  let metaLoaded = $state(false)

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

  // Model suggestions depend on the chosen agent. See profileedit.ts for the
  // agent-id probing (legacy `claude-code` -> `claude`) and fallback rules.
  // A model already paired with the same agent by another group is still
  // listed, marked "in use", and disabled - never filtered out.
  function modelOptionsFor(i: number) {
    const taken = new Set(disabledModelIds(i, groups))
    return modelIdsForAgent(groups[i].agent, agents, modelsTable).map((id) => ({
      value: id,
      hint: taken.has(id) ? 'in use' : undefined,
    }))
  }

  // Fired only when the user actually picks an agent in the combobox (see
  // Combobox's `onChange`), never when a group is assigned programmatically
  // by loadProfile/loadMeta below - those must preserve a saved profile's
  // model as-is, not reset it to the new agent's first option. The reset picks
  // the first SELECTABLE model, so switching agent can never land the group on
  // a pair another group already holds.
  function onAgentChange(i: number, v: string) {
    groups[i].model = firstSelectableModelForAgent(v, i, groups, agents, modelsTable)
  }

  function addFallback() {
    // Unreachable from the UI while the metadata loads (the button is
    // disabled), kept as a backstop so a programmatic call cannot append a row
    // computed from an empty agent list.
    if (!metaLoaded) return
    const pair = nextFallbackPair(groups, agents, modelsTable)
    if (!pair) return toast.error('No agent and model combination is left to add')
    groups = [...groups, pair]
  }

  function removeFallback(i: number) {
    groups = groups.filter((_, j) => j !== i)
  }

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

  // Pre-fill the primary executor of a NEW profile once the metadata is known,
  // so the form opens on a usable pair instead of an empty model that only
  // fails at save time. Guarded by `isNew`, so it can never run on the load
  // path of an existing profile, where the saved model must survive verbatim.
  // Only empty fields are written, so a value the user already picked stays.
  function applyNewProfileDefaults() {
    if (!isNew || !metaLoaded) return
    if (groups[0].agent && groups[0].model) return
    const pair = defaultPrimaryPair(groups, agents, modelsTable)
    if (!groups[0].agent) groups[0].agent = pair.agent
    if (!groups[0].model && groups[0].agent === pair.agent) groups[0].model = pair.model
  }

  async function loadMeta() {
    metaLoaded = false
    try {
      const [pj, ag, md] = await Promise.all([fetchProjects(), fetchAgents(), fetchModels()])
      projects = pj
      agents = ag
      modelsTable = md.models
      if (isNew && !workspaceInput && projects.length) workspaceInput = projects[0].workspace_id
    } catch (e) {
      toast.error('Failed to load form data', { description: String(e) })
    } finally {
      // Settled either way: a failed fetch leaves a genuinely empty agent list,
      // which the executor guards should report rather than keep spinning on.
      metaLoaded = true
      applyNewProfileDefaults()
    }
  }

  async function loadProfile(token: number) {
    if (isNew) return
    try {
      const d = await fetchProfile(name, scope, workspace)
      if (token !== loadToken) return
      expectedDigest = d.profile_digest
      soul = d.soul_md
      const parsed = parseProfileDoc(d.profile_yaml)
      // Assigned as data, so no Combobox `onChange` fires and the saved models
      // survive verbatim - including a fallback whose model is not first in
      // its agent's list.
      groups = [{ agent: parsed.agent, model: parsed.model }, ...parsed.fallbacks]
      description = parsed.description
      skills = parsed.skills
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
    // Drop the fallbacks and the model but keep the primary agent, so a new
    // profile still shows the default agent loadMeta picked once at mount.
    // `untrack` keeps this write from making the effect depend on `groups`.
    groups = [{ agent: untrack(() => groups[0]?.agent ?? ''), model: '' }]
    description = ''
    skills = []
    // Re-seed the primary pair when the new target is a new profile and the
    // metadata already arrived (loadMeta runs once at mount, so a later route
    // change gets no second chance from it). `untrack` keeps this effect from
    // depending on the metadata state and re-running the profile load.
    untrack(applyNewProfileDefaults)
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
    for (const [i, g] of groups.entries()) {
      const where = i === 0 ? '' : ` (fallback ${i})`
      if (!g.agent.trim()) return toast.error(`Agent is required${where}`)
      if (!g.model.trim()) return toast.error(`Model is required${where}`)
    }
    // The combobox accepts arbitrary typed values, so a duplicate pair can
    // still be produced by hand. Refuse it here rather than sending it.
    const dup = findDuplicatePair(groups)
    if (dup) {
      return toast.error(`Duplicate executor: ${dup.agent} / ${dup.model} is used more than once`)
    }
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
          agent: groups[0].agent.trim(),
          model: groups[0].model.trim(),
          fallbacks: groups.slice(1).map((g) => ({ agent: g.agent.trim(), model: g.model.trim() })),
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

      <Field.FieldSet>
        <Field.FieldLegend variant="label">Executor</Field.FieldLegend>
        <Field.FieldDescription>
          The primary agent and model, then the fallbacks tried in order when a step fails. Every
          entry keeps this profile's SOUL and skills, only the executor changes.
        </Field.FieldDescription>
        <Field.FieldGroup>
          {#each groups as _g, i}
            <Field.Field>
              <div class="rounded-md border border-border bg-muted/30 p-3">
                <Field.FieldGroup class="grid gap-3 sm:grid-cols-2">
                  <Field.Field>
                    <!-- The group identity lives in the agent label (there is no
                         separate group caption); the matching min-h keeps the two
                         columns aligned whether or not a Remove button is shown. -->
                    <div class="flex min-h-8 items-center gap-2">
                      <Field.FieldLabel for={`pf-agent-${i}`}>
                        {i === 0 ? 'Primary Agent' : `Fallback Agent ${i}`}
                      </Field.FieldLabel>
                      {#if i > 0}
                        <!-- Below sm the fields stack, so the Remove button moves up to
                             this row (top-right of the group); from sm up the model-row
                             instance below takes over. sm:hidden / hidden sm:inline-flex
                             keep exactly one of the pair visible at any width. -->
                        <Button
                          variant="ghost"
                          size="sm"
                          class="ml-auto max-sm:px-2 text-muted-foreground hover:text-destructive sm:hidden"
                          onclick={() => removeFallback(i)}
                        >
                          <Trash2 data-icon="inline-start" />
                          <span class="max-sm:sr-only">Remove</span>
                        </Button>
                      {/if}
                    </div>
                    <Combobox
                      id={`pf-agent-${i}`}
                      bind:value={groups[i].agent}
                      options={agentOptions}
                      onChange={(v) => onAgentChange(i, v)}
                      placeholder="Select an agent"
                      emptyText="No installed agents"
                    />
                  </Field.Field>
                  <Field.Field>
                    <div class="flex min-h-8 items-center gap-2">
                      <Field.FieldLabel for={`pf-model-${i}`}>Model</Field.FieldLabel>
                      {#if i > 0}
                        <Button
                          variant="ghost"
                          size="sm"
                          class="ml-auto max-sm:px-2 text-muted-foreground hover:text-destructive hidden sm:inline-flex"
                          onclick={() => removeFallback(i)}
                        >
                          <Trash2 data-icon="inline-start" />
                          <span class="max-sm:sr-only">Remove</span>
                        </Button>
                      {/if}
                    </div>
                    <Combobox
                      id={`pf-model-${i}`}
                      bind:value={groups[i].model}
                      options={modelOptionsFor(i)}
                      disabledValues={disabledModelIds(i, groups)}
                      placeholder="Select a model"
                      emptyText="No models for this agent"
                    />
                  </Field.Field>
                </Field.FieldGroup>
              </div>
            </Field.Field>
          {/each}
        </Field.FieldGroup>
        <!-- Disabled until the agent detection call lands: before that
             `nextFallbackPair` can only fail, which used to surface as a bogus
             "nothing left to add" toast. -->
        <Button
          variant="outline"
          size="sm"
          class="self-start"
          onclick={addFallback}
          disabled={!metaLoaded}
        >
          {#if metaLoaded}
            <Plus data-icon="inline-start" />
            Add fallback
          {:else}
            <Spinner data-icon="inline-start" />
            Loading agents...
          {/if}
        </Button>
      </Field.FieldSet>

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
