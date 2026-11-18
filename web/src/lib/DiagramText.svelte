<script lang="ts">
  import { toast } from 'svelte-sonner'

  // people-in-the-loop (D6): the diagram text beside the graph, the
  // definition's Mermaid flowchart as text, collapsed until opened. Plain
  // text and a copy button; no diagram library renders it.
  let { text }: { text: string } = $props()

  let copied = $state(false)
  async function copy() {
    try {
      await navigator.clipboard.writeText(text)
      copied = true
      setTimeout(() => (copied = false), 1500)
    } catch {
      toast.error('Could not copy; select the text instead')
    }
  }
</script>

<details class="diagram-text" data-testid="diagram-text">
  <summary>Diagram text</summary>
  <!-- One box holds the copy button and the text, so the button never
       sits on the canvas outside it. -->
  <div class="diagram-body" data-testid="diagram-box">
    <button type="button" class="diagram-copy" onclick={() => void copy()}>
      {copied ? 'copied' : 'copy'}
    </button>
    <pre>{text}</pre>
  </div>
</details>

<style>
  /* Placed in the flow of the layers panel, below its bar: the panel
     bounds its width and the `pre` scrolls, so the block never paints
     over the layer toggles or the zoom controls. Plain CSS over the app's
     variables. */
  .diagram-text {
    max-width: 100%;
    font-size: 11px;
  }
  .diagram-text summary {
    display: inline-block;
    padding: 2px 8px;
    border: 1px solid var(--c-line-2);
    background: var(--c-bg);
    color: var(--c-fg-2);
    cursor: pointer;
    list-style: revert;
  }
  .diagram-text summary:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .diagram-body {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 4px;
    padding: 4px;
    border: 1px solid var(--c-line);
    background: var(--c-bg);
  }
  .diagram-copy {
    align-self: flex-end;
    padding: 2px 8px;
    border: 1px solid var(--c-line-2);
    background: var(--c-bg);
    color: var(--c-fg-2);
    font: inherit;
    cursor: pointer;
  }
  .diagram-copy:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .diagram-text pre {
    margin: 0;
    max-height: 40vh;
    overflow: auto;
    padding: 2px 4px;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    white-space: pre;
  }
</style>
