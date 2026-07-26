<script lang="ts">
  // The small "i" beside a field label: what the field does, in one or two
  // sentences.
  //
  // A popover rather than a tooltip, because a tooltip only answers a mouse: a
  // popover opens on tap as well, dismisses by tapping outside or pressing
  // Escape, and still opens on hover for a pointer (bits-ui ignores touch
  // pointers for the hover path, so a tap never fires both).
  import * as Popover from '$lib/components/ui/popover'
  import Info from '@lucide/svelte/icons/info'

  let { text, label }: { text: string; label: string } = $props()
</script>

<Popover.Root>
  <Popover.Trigger
    openOnHover
    openDelay={150}
    closeDelay={100}
    aria-label={`About ${label}`}
    class="inline-flex size-4 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
  >
    <Info class="size-3.5" />
  </Popover.Trigger>
  <Popover.Content
    side="top"
    class="w-72 max-w-[calc(100vw-2rem)] gap-0 p-3 text-xs leading-relaxed font-normal"
  >
    {text}
  </Popover.Content>
</Popover.Root>
