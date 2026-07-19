<script lang="ts">
  // The connector detail page's manual "call a function" playground (spec
  // 2026-07-19-official-connectors-design section 7). Pick a function, get a
  // form generated from its args_schema (or a raw-JSON textarea for a
  // complex schema), pick an account, toggle dry-run vs real, and see the
  // structured result. Thin component: all form generation, arg coercion,
  // and result formatting live in `../connectorplay` (pure, unit-tested).
  import { callConnector } from '../api'
  import type { ConnectorAccount, ConnectorFunction } from '../connectors'
  import {
    buildPlayFields,
    coerceFormValues,
    formatResultBody,
    isSimpleObjectSchema,
    parseRawArgs,
    resultSummary,
    type PlayCallResult,
  } from '../connectorplay'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import * as Card from '$lib/components/ui/card'
  import * as Field from '$lib/components/ui/field'
  import * as Select from '$lib/components/ui/select'
  import { Switch } from '$lib/components/ui/switch'
  import { Input } from '$lib/components/ui/input'
  import { Textarea } from '$lib/components/ui/textarea'
  import { Spinner } from '$lib/components/ui/spinner'
  import { toast } from 'svelte-sonner'
  import FlaskConical from '@lucide/svelte/icons/flask-conical'
  import Play from '@lucide/svelte/icons/play'

  let {
    name,
    workspace = '',
    functions,
    accounts,
  }: {
    name: string
    workspace?: string
    functions: ConnectorFunction[]
    accounts: ConnectorAccount[]
  } = $props()

  let selectedFunction = $state('')
  let selectedAccount = $state('')
  let dryRun = $state(true)
  let rawMode = $state(false)
  let formValues = $state<Record<string, string | boolean>>({})
  let rawArgsText = $state('')
  let argsError = $state<string | null>(null)
  let calling = $state(false)
  let result = $state<PlayCallResult | null>(null)

  const currentFunction = $derived(functions.find((f) => f.name === selectedFunction) ?? null)
  const schema = $derived(currentFunction?.argsSchema ?? null)
  const fields = $derived(buildPlayFields(schema))
  const simpleOk = $derived(isSimpleObjectSchema(schema))
  // A complex schema always falls back to raw JSON; a simple one honors the
  // manual override switch (resolved ambiguities: automatic AND overridable).
  const useRaw = $derived(!simpleOk || rawMode)

  // Effect 1: keep the function selection valid as `functions` loads or changes.
  $effect(() => {
    if (!functions.some((f) => f.name === selectedFunction)) {
      selectedFunction = functions[0]?.name ?? ''
    }
  })

  // Effect 2: preselect the default account, mirroring the CLI's single/default rule.
  $effect(() => {
    if (!accounts.some((a) => a.name === selectedAccount)) {
      selectedAccount = accounts.find((a) => a.default)?.name ?? accounts[0]?.name ?? ''
    }
  })

  // Effect 3: reset the form and any previous result when the function changes.
  $effect(() => {
    void selectedFunction
    formValues = {}
    rawArgsText = ''
    argsError = null
    result = null
    rawMode = false
  })

  function setField(fieldName: string, value: string | boolean) {
    formValues = { ...formValues, [fieldName]: value }
  }

  async function call() {
    argsError = null
    let args: Record<string, unknown>
    try {
      args = useRaw ? parseRawArgs(rawArgsText) : coerceFormValues(fields, formValues)
    } catch (e) {
      argsError = e instanceof Error ? e.message : String(e)
      return
    }
    calling = true
    try {
      result = await callConnector(
        name,
        {
          function: selectedFunction,
          account: selectedAccount || null,
          args,
          dryRun,
        },
        workspace,
      )
      if (!result.ok) {
        toast.error('Call failed', { description: result.error?.message ?? 'unknown error' })
      }
    } catch (e) {
      toast.error('Call request failed', { description: String(e) })
    } finally {
      calling = false
    }
  }

  const badgeClass = {
    ok: 'border-success/30 bg-success/15 text-success',
    warn: 'border-warning/30 bg-warning/15 text-warning',
    danger: 'border-destructive/30 bg-destructive/15 text-destructive',
    muted: '',
  } as const

  const resultTone = $derived.by((): keyof typeof badgeClass => {
    if (!result) return 'muted'
    if (!result.ok) return 'danger'
    return result.dry_run ? 'muted' : 'ok'
  })

  const canCall = $derived(
    !calling && selectedFunction !== '' && (accounts.length === 0 || selectedAccount !== ''),
  )
</script>

<Card.Root>
  <Card.Header>
    <div class="flex items-center gap-2">
      <FlaskConical class="size-4 text-muted-foreground" />
      <Card.Title class="text-sm">Playground</Card.Title>
    </div>
    <Card.Description>
      Call a function manually. Dry run renders the request without touching secrets; a real
      call is trust-gated the same way as the healthcheck probe.
    </Card.Description>
  </Card.Header>
  <Card.Content class="flex flex-col gap-4">
    {#if functions.length === 0}
      <p class="text-sm text-muted-foreground">This connector declares no functions.</p>
    {:else}
      <Field.FieldGroup class="gap-3">
        <Field.Field>
          <Field.FieldLabel for="pg-function">Function</Field.FieldLabel>
          <Select.Root type="single" bind:value={selectedFunction}>
            <Select.Trigger id="pg-function" class="w-full font-mono text-xs">
              {selectedFunction || 'select a function'}
            </Select.Trigger>
            <Select.Content>
              <Select.Group>
                {#each functions as f (f.name)}
                  <Select.Item value={f.name} label={f.name}>{f.name}</Select.Item>
                {/each}
              </Select.Group>
            </Select.Content>
          </Select.Root>
          {#if currentFunction?.description}
            <Field.FieldDescription>{currentFunction.description}</Field.FieldDescription>
          {/if}
        </Field.Field>

        <Field.Field>
          <Field.FieldLabel for="pg-account">Account</Field.FieldLabel>
          {#if accounts.length === 0}
            <p class="text-sm text-muted-foreground">
              No accounts configured for this connector.
            </p>
          {:else}
            <Select.Root type="single" bind:value={selectedAccount}>
              <Select.Trigger id="pg-account" class="w-full font-mono text-xs">
                {selectedAccount || 'select an account'}
              </Select.Trigger>
              <Select.Content>
                <Select.Group>
                  {#each accounts as a (a.name)}
                    <Select.Item value={a.name} label={a.name}>
                      {a.name}{a.default ? ' (default)' : ''}
                    </Select.Item>
                  {/each}
                </Select.Group>
              </Select.Content>
            </Select.Root>
          {/if}
        </Field.Field>

        <Field.Field orientation="horizontal">
          <Switch id="pg-dry-run" bind:checked={dryRun} />
          <Field.FieldLabel for="pg-dry-run" class="font-normal">Dry run</Field.FieldLabel>
        </Field.Field>

        {#if simpleOk}
          <Field.Field orientation="horizontal">
            <Switch id="pg-raw" bind:checked={rawMode} />
            <Field.FieldLabel for="pg-raw" class="font-normal">Raw JSON</Field.FieldLabel>
          </Field.Field>
        {/if}

        {#if useRaw}
          <Field.Field>
            <Field.FieldLabel for="pg-raw-args">Args (JSON)</Field.FieldLabel>
            <Textarea
              id="pg-raw-args"
              bind:value={rawArgsText}
              rows={4}
              class="font-mono text-sm"
              placeholder={'{}'}
            />
          </Field.Field>
        {:else if fields.length > 0}
          {#each fields as field (field.name)}
            <Field.Field>
              <Field.FieldLabel for={`pg-f-${field.name}`}>
                {field.name}{#if field.required}<span class="text-destructive"> *</span>{/if}
              </Field.FieldLabel>
              {#if field.description}
                <Field.FieldDescription>{field.description}</Field.FieldDescription>
              {/if}
              {#if field.kind === 'boolean'}
                <Switch
                  id={`pg-f-${field.name}`}
                  checked={formValues[field.name] === true}
                  onCheckedChange={(v) => setField(field.name, v)}
                />
              {:else if field.kind === 'enum'}
                <Select.Root
                  type="single"
                  value={String(formValues[field.name] ?? '')}
                  onValueChange={(v) => setField(field.name, v ?? '')}
                >
                  <Select.Trigger id={`pg-f-${field.name}`} class="w-full">
                    {formValues[field.name] || 'select a value'}
                  </Select.Trigger>
                  <Select.Content>
                    <Select.Group>
                      {#each field.enumValues ?? [] as ev (ev)}
                        <Select.Item value={String(ev)} label={String(ev)}>{ev}</Select.Item>
                      {/each}
                    </Select.Group>
                  </Select.Content>
                </Select.Root>
              {:else}
                <Input
                  id={`pg-f-${field.name}`}
                  value={String(formValues[field.name] ?? '')}
                  oninput={(e) => setField(field.name, e.currentTarget.value)}
                  type={field.kind === 'number' ? 'number' : 'text'}
                />
              {/if}
            </Field.Field>
          {/each}
        {/if}

        {#if argsError}
          <p class="text-sm text-destructive">{argsError}</p>
        {/if}
      </Field.FieldGroup>

      <div>
        <Button size="sm" onclick={call} disabled={!canCall}>
          {#if calling}<Spinner data-icon="inline-start" />{:else}<Play data-icon="inline-start" />{/if}
          {dryRun ? 'Render' : 'Call'}
        </Button>
      </div>

      {#if result}
        <div class="flex flex-col gap-2 rounded-md border border-border p-3">
          <div class="flex flex-wrap items-center gap-2">
            <Badge variant="outline" class={badgeClass[resultTone]}>{resultSummary(result)}</Badge>
            {#if result.picked}
              <Badge variant="secondary">picked</Badge>
            {/if}
            {#if result.truncated}
              <Badge variant="outline" class={badgeClass.warn}>truncated</Badge>
            {/if}
          </div>
          {#if result.link}
            <p class="break-all font-mono text-xs text-muted-foreground">link: {result.link}</p>
          {/if}
          <pre class="max-h-80 overflow-auto rounded bg-muted p-2 font-mono text-xs">{formatResultBody(result)}</pre>
        </div>
      {/if}
    {/if}
  </Card.Content>
</Card.Root>
