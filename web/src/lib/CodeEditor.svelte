<script lang="ts">
  // Simple text editor on a <textarea>. Syntax highlighting and a code editor
  // (CodeMirror) are intentionally NOT wired in - not needed yet. The component
  // interface (value/onChange/readonly) is stable: the internals can be
  // swapped for CodeMirror later without changing the calling code.
  let {
    value,
    onChange,
    language: _language = 'yaml',
    readonly = false,
  }: {
    value: string
    onChange: (v: string) => void
    language?: 'yaml' | 'text'
    readonly?: boolean
  } = $props()
</script>

<textarea
  class="size-full resize-none whitespace-pre bg-background p-3 font-mono text-[13px] leading-relaxed text-foreground outline-none"
  style="tab-size: 2;"
  spellcheck="false"
  autocomplete="off"
  autocapitalize="off"
  {readonly}
  {value}
  oninput={(e) => onChange(e.currentTarget.value)}
></textarea>
