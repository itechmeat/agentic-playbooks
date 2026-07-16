<script lang="ts">
  import { cn } from '$lib/utils'
  import * as Popover from '$lib/components/ui/popover'
  import { Input } from '$lib/components/ui/input'
  import { buttonVariants } from '$lib/components/ui/button'
  import Check from '@lucide/svelte/icons/check'
  import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down'

  type Option = { value: string; label?: string; hint?: string }
  type Row = { kind: 'option' | 'custom'; value: string; label: string; hint?: string }

  let {
    value = $bindable(''),
    options = [],
    placeholder = 'Select...',
    emptyText = 'No matches',
    allowCustom = true,
    id,
  }: {
    value?: string
    options?: Option[]
    placeholder?: string
    emptyText?: string
    allowCustom?: boolean
    id?: string
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
  const rows = $derived.by<Row[]>(() => {
    const q = search.trim()
    const rs: Row[] = filtered.map((o) => ({
      kind: 'option',
      value: o.value,
      label: o.label ?? o.value,
      hint: o.hint,
    }))
    if (allowCustom && q && !exactMatch) rs.push({ kind: 'custom', value: q, label: `Use "${q}"` })
    return rs
  })

  // Reset the highlight to the first row whenever the popover opens or the
  // filtered list changes, so it never points past the end of a shrunk list.
  $effect(() => {
    void open
    highlighted = rows.length ? 0 : -1
  })

  function choose(v: string) {
    value = v
    open = false
    search = ''
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
      if (rows.length) highlighted = Math.min(highlighted + 1, rows.length - 1)
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      if (rows.length) highlighted = Math.max(highlighted - 1, 0)
    } else if (e.key === 'Enter') {
      e.preventDefault()
      const row = rows[highlighted]
      if (row) choose(row.value)
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
          class={cn(
            'flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm',
            i === highlighted
              ? 'bg-accent text-accent-foreground'
              : 'hover:bg-accent hover:text-accent-foreground',
          )}
          onmousemove={() => (highlighted = i)}
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
