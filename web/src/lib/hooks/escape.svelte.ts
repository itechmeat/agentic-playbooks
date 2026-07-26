/**
 * Dismiss-on-Escape for panels that are built out of plain elements.
 *
 * The overlays over a playbook canvas (yaml, diff, the node form, the version
 * history) are ordinary markup, so nothing closes them but their own close
 * button. This gives them the key everyone reaches for.
 *
 * The listener sits on `window`, downstream of the document-level listener
 * bits-ui installs for its own layers, and it ignores an event that has
 * already been handled: a dialog, a select or a popover calls preventDefault
 * when it is the layer that should close, so Escape inside an open select
 * closes the select and leaves the panel behind it standing.
 *
 * Call once per component and close the topmost layer in the handler - the
 * component knows its own stacking order, this does not.
 */
export function onEscape(close: () => void): void {
  $effect(() => {
    const handle = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return
      close()
    }
    window.addEventListener('keydown', handle)
    return () => window.removeEventListener('keydown', handle)
  })
}
