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
  class="editor"
  spellcheck="false"
  autocomplete="off"
  autocapitalize="off"
  {readonly}
  {value}
  oninput={(e) => onChange(e.currentTarget.value)}
></textarea>

<style>
  .editor {
    width: 100%;
    height: 100%;
    resize: none;
    border: 0;
    outline: none;
    padding: 10px 12px;
    box-sizing: border-box;
    background: var(--bg);
    color: var(--fg);
    font-size: 13px;
    line-height: 1.5;
    tab-size: 2;
    font-family: ui-monospace, 'Cascadia Code', 'Source Code Pro', Menlo, monospace;
    white-space: pre;
    overflow: auto;
  }
</style>
