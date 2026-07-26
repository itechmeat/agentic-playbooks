<script lang="ts">
  // YAML editor. The visible editor is CodeMirror 6, loaded on demand: it is
  // only mounted inside a yaml overlay, so its grammar and view code stay out
  // of the initial bundle and arrive the first time someone opens one.
  //
  // Until that chunk lands - and if the import fails outright - the component
  // stays the plain textarea it always was: same value, same onChange, no
  // highlighting. Editing never depends on the chunk arriving.
  //
  // The component interface (value/onChange/readonly) is unchanged; `readonly`
  // now also blocks CodeMirror's own edits, not just the textarea's.
  import { onMount, untrack } from 'svelte'
  import type { EditorView } from '@codemirror/view'

  let {
    value,
    onChange,
    language: _language = 'yaml',
    readonly = false,
  }: {
    value: string
    /// Absent for a read-only view, which has nothing to report.
    onChange?: (v: string) => void
    language?: 'yaml' | 'text'
    readonly?: boolean
  } = $props()

  let host = $state<HTMLDivElement | null>(null)
  let view = $state<EditorView | null>(null)

  onMount(() => {
    let disposed = false
    let created: EditorView | null = null

    void (async () => {
      const [cmView, cmState, cmLang, cmCommands, cmYaml, lezerHighlight] = await Promise.all([
        import('@codemirror/view'),
        import('@codemirror/state'),
        import('@codemirror/language'),
        import('@codemirror/commands'),
        import('@codemirror/lang-yaml'),
        import('@lezer/highlight'),
      ])
      if (disposed || !host) return

      const { EditorView: View, keymap, highlightActiveLine, lineNumbers } = cmView
      const { EditorState } = cmState
      const { HighlightStyle, syntaxHighlighting, indentUnit, bracketMatching, foldGutter } = cmLang
      const { defaultKeymap, history, historyKeymap, indentWithTab } = cmCommands
      const { tags } = lezerHighlight

      // Token colors come from CSS variables (defined in app.css) so the editor
      // follows the app's light/dark theme instead of shipping its own palette.
      const highlight = HighlightStyle.define([
        { tag: tags.keyword, color: 'var(--cm-key)' },
        { tag: tags.definition(tags.propertyName), color: 'var(--cm-key)' },
        { tag: tags.propertyName, color: 'var(--cm-key)' },
        { tag: tags.string, color: 'var(--cm-string)' },
        { tag: tags.number, color: 'var(--cm-number)' },
        { tag: tags.bool, color: 'var(--cm-number)' },
        { tag: tags.null, color: 'var(--cm-number)' },
        { tag: tags.atom, color: 'var(--cm-number)' },
        { tag: tags.comment, color: 'var(--cm-comment)', fontStyle: 'italic' },
        { tag: tags.meta, color: 'var(--cm-meta)' },
        { tag: tags.punctuation, color: 'var(--cm-punct)' },
        { tag: tags.separator, color: 'var(--cm-punct)' },
        { tag: tags.invalid, color: 'var(--destructive)' },
      ])

      created = new View({
        parent: host,
        state: EditorState.create({
          doc: untrack(() => value),
          extensions: [
            lineNumbers(),
            foldGutter(),
            history(),
            bracketMatching(),
            highlightActiveLine(),
            indentUnit.of('  '),
            keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
            cmYaml.yaml(),
            syntaxHighlighting(highlight),
            View.lineWrapping,
            // Both are needed: `readOnly` refuses edits, `editable` also drops
            // the caret and the editing affordances, so a read-only view reads
            // as text rather than as an input that silently ignores you.
            EditorState.readOnly.of(readonly),
            View.editable.of(!readonly),
            View.updateListener.of((u) => {
              if (u.docChanged) onChange?.(u.state.doc.toString())
            }),
          ],
        }),
      })
      view = created
    })()

    return () => {
      disposed = true
      created?.destroy()
      view = null
    }
  })

  // The graph edits the same document (add a node, drag one, delete an edge),
  // so an external change has to reach the editor - but only when it really
  // differs, otherwise every keystroke would round-trip back and drop the
  // cursor.
  $effect(() => {
    const next = value
    const v = untrack(() => view)
    if (!v) return
    const current = v.state.doc.toString()
    if (current === next) return
    v.dispatch({ changes: { from: 0, to: current.length, insert: next } })
  })
</script>

<div bind:this={host} class="cm-host size-full min-h-0 overflow-hidden">
  {#if !view}
    <textarea
      class="size-full resize-none whitespace-pre bg-background p-3 font-mono text-[13px] leading-relaxed text-foreground outline-none"
      style="tab-size: 2;"
      spellcheck="false"
      autocomplete="off"
      autocapitalize="off"
      {readonly}
      {value}
      oninput={(e) => onChange?.(e.currentTarget.value)}
    ></textarea>
  {/if}
</div>
