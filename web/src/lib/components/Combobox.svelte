<script lang="ts">
  import { cn } from '$lib/utils'
  import * as Popover from '$lib/components/ui/popover'
  import { Input } from '$lib/components/ui/input'
  import { buttonVariants } from '$lib/components/ui/button'
  import Check from '@lucide/svelte/icons/check'
  import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down'

  type Option = { value: string; label?: string; hint?: string }
  type Row = {
    kind: 'option' | 'custom'
    value: string
    label: string
    hint?: string
    disabled: boolean
  }

  let {
    value = $bindable(''),
    options = [],
    placeholder = 'Select...',
    emptyText = 'No matches',
    allowCustom = true,
    disabledValues = [],
    id,
    onChange,
  }: {
    value?: string
    options?: Option[]
    placeholder?: string
    emptyText?: string
    allowCustom?: boolean
    // Option values that stay visible but cannot be chosen: skipped by the
    // arrow keys, ignored by Enter, and rendered as disabled buttons so a
    // click or a Tab can never reach them. Defaults to none, so existing
    // callers are unaffected.
    disabledValues?: string[]
    id?: string
    // Fired only when the user picks a row (click or Enter), never when
    // `value` changes for any other reason (e.g. a caller assigning it
    // programmatically). Callers that need to react to a genuine user
    // selection - as opposed to a value set from loaded data - should use
    // this instead of an effect watching `value`.
    onChange?: (value: string) => void
  } = $props()

  let open = $state(false)
  let search = $state('')
  // Index into `rows` of the keyboard-highlighted option (-1 when the list is
  // empty). Mouse hover and arrow keys move it; Enter chooses it.
  let highlighted = $state(0)

  const filtered = $derived(
    search.trim()
      ? options.filter((o) =>
          `${o.value} ${o.label ?? ''}`.toLowerCase().includes(search.trim().toLowerCase()),
        )
      : options,
  )
  const exactMatch = $derived(
    options.some((o) => o.value.toLowerCase() === search.trim().toLowerCase()),
  )

  // The single navigable list: matching options plus, when custom values are
  // allowed and the typed text is not already an option, a trailing "use this"
  // entry. Keeping both in one array means arrow keys and Enter treat the
  // custom entry exactly like an option.
  const disabledSet = $derived(new Set(disabledValues))

  const rows = $derived.by<Row[]>(() => {
    const q = search.trim()
    const rs: Row[] = filtered.map((o) => ({
      kind: 'option',
      value: o.value,
      label: o.label ?? o.value,
      hint: o.hint,
      disabled: disabledSet.has(o.value),
    }))
    if (allowCustom && q && !exactMatch)
      rs.push({ kind: 'custom', value: q, label: `Use "${q}"`, disabled: false })
    return rs
  })

  // The nearest selectable row at or after `from`, walking in `dir`; falls back
  // to the current highlight when the scan runs off either end, so a run of
  // disabled rows never strands the keyboard cursor on one of them.
  function nextEnabled(from: number, dir: 1 | -1): number {
    for (let i = from; i >= 0 && i < rows.length; i += dir) {
      if (!rows[i].disabled) return i
    }
    return highlighted
  }

  // Reset the highlight to the first selectable row whenever the popover opens
  // or the filtered list changes, so it never points past the end of a shrunk
  // list nor lands on a disabled row.
  $effect(() => {
    void open
    highlighted = rows.findIndex((r) => !r.disabled)
  })

  function choose(v: string) {
    if (disabledSet.has(v)) return
    value = v
    open = false
    search = ''
    onChange?.(v)
  }

  // Clear the filter on close so a reopen never shows a stale search.
  function onOpenChange(o: boolean) {
    if (!o) {
      search = ''
      highlighted = 0
    }
  }

  function onSearchKeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      if (rows.length) highlighted = nextEnabled(highlighted + 1, 1)
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      if (rows.length) highlighted = nextEnabled(highlighted - 1, -1)
    } else if (e.key === 'Enter') {
      e.preventDefault()
      const row = rows[highlighted]
      if (row && !row.disabled) choose(row.value)
    }
  }
</script>

<Popover.Root bind:open {onOpenChange}>
  <Popover.Trigger
    {id}
    class={cn(buttonVariants({ variant: 'outline' }), 'w-full justify-between font-normal')}
  >
    <span class={cn('truncate', !value && 'text-muted-foreground')}>{value || placeholder}</span>
    <ChevronsUpDown class="size-4 shrink-0 opacity-50" />
  </Popover.Trigger>
  <Popover.Content class="w-64 p-0" align="start">
    <div class="border-b border-border p-2">
      <!-- svelte-ignore a11y_autofocus -->
      <Input
        bind:value={search}
        placeholder={allowCustom ? 'Search or type a value...' : 'Search...'}
        class="h-8"
        autofocus
        role="combobox"
        aria-expanded="true"
        aria-controls="apb-combobox-list"
        onkeydown={onSearchKeydown}
      />
    </div>
    <div id="apb-combobox-list" role="listbox" class="max-h-60 overflow-auto p-1">
      {#each rows as row, i (row.kind + ':' + row.value)}
        <button
          type="button"
          role="option"
          aria-selected={i === highlighted}
          aria-disabled={row.disabled}
          disabled={row.disabled}
          class={cn(
            'flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm',
            row.disabled
              ? 'cursor-not-allowed text-muted-foreground opacity-50'
              : i === highlighted
                ? 'bg-accent text-accent-foreground'
                : 'hover:bg-accent hover:text-accent-foreground',
          )}
          onmousemove={() => {
            if (!row.disabled) highlighted = i
          }}
          onclick={() => choose(row.value)}
        >
          <Check
            class={cn(
              'size-4 shrink-0',
              row.kind === 'option' && value === row.value ? 'opacity-100' : 'opacity-0',
            )}
          />
          <span class="truncate">{row.label}</span>
          {#if row.hint}<span class="ml-auto text-xs text-muted-foreground">{row.hint}</span>{/if}
        </button>
      {/each}
      {#if rows.length === 0}
        <div class="px-2 py-6 text-center text-sm text-muted-foreground">{emptyText}</div>
      {/if}
    </div>
  </Popover.Content>
</Popover.Root>
