<script lang="ts">
  import type { Snippet } from 'svelte'
  import { cn } from '$lib/utils'

  let { class: className, children }: { class?: string; children: Snippet } = $props()

  let el = $state<HTMLElement | null>(null)

  // `#app` is a fixed-height flex column, so the document itself never
  // scrolls and this element is the page's only vertical scroll container.
  //
  // A plain div is not a keyboard scroll target until something focuses it,
  // and nothing did: on a fresh load `document.activeElement` was `body`, and
  // the body cannot scroll here, so PageDown, Space, Home, End, and the arrow
  // keys all did nothing. Scrolling only started working once the user
  // happened to click somewhere inside the page.
  //
  // `tabindex="-1"` makes the region focusable without adding a tab stop, and
  // taking that focus on mount makes the keys work immediately, before any
  // click. Programmatic focus does not match `:focus-visible`, so no ring is
  // drawn on load.
  $effect(() => {
    const node = el
    if (!node) return
    // Never steal focus from a control that has already claimed it, such as a
    // field an editor page autofocuses.
    if (document.activeElement && document.activeElement !== document.body) return
    node.focus({ preventScroll: true })
  })

  // Clicking a spot that cannot take focus (card padding, the page background
  // beside a card) drops focus back to the body, which cannot scroll here, so
  // the keys would go dead again after the first stray click. A focusout that
  // names no new target is exactly that case: take the focus back, and leave
  // every real control alone, since those arrive as `relatedTarget`.
  function keepScrollTarget(e: FocusEvent) {
    if (e.relatedTarget !== null) return
    const node = el
    if (!node || !node.isConnected) return
    node.focus({ preventScroll: true })
  }
</script>

<!-- `main` rather than a div: this really is the page's main content, so the
     landmark comes for free and screen readers can jump straight to it. -->
<main
  bind:this={el}
  tabindex="-1"
  onfocusout={keepScrollTarget}
  class={cn('min-h-0 flex-1 overflow-auto', className)}
>
  {@render children()}
</main>
