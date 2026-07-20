<script lang="ts">
  import * as Card from '$lib/components/ui/card'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'
  import type { QuestionEntry } from './questions'

  // Mirrors RunView's "Human review" panel interaction pattern: option
  // buttons for a suggested answer plus a free-text field, both disabled
  // while an answer is in flight. The question text is rendered as plain text
  // ({expression}, never {@html}) since it comes verbatim from the agent and
  // must never be interpreted as markdown or HTML.
  let {
    question,
    posting = false,
    onAnswer,
  }: {
    question: QuestionEntry
    posting?: boolean
    onAnswer: (node: string, answer: string) => void | Promise<void>
  } = $props()

  let freeText = $state('')

  function submitOption(opt: string) {
    if (posting) return
    void onAnswer(question.node, opt)
  }

  function submitFreeText(e: SubmitEvent) {
    e.preventDefault()
    const value = freeText.trim()
    if (!value || posting) return
    void onAnswer(question.node, value)
    freeText = ''
  }
</script>

<Card.Root class="border-primary/60">
  <Card.Header>
    <Card.Title class="text-sm">Question</Card.Title>
  </Card.Header>
  <Card.Content class="flex flex-col gap-3">
    <div class="flex flex-col gap-1">
      <span class="font-mono text-xs">{question.node}</span>
      <p class="text-sm">{question.question}</p>
    </div>

    {#if question.options.length}
      <div class="flex flex-wrap gap-1">
        {#each question.options as opt (opt)}
          <Button
            variant="outline"
            size="sm"
            class="h-7"
            onclick={() => submitOption(opt)}
            disabled={posting}
          >
            {opt}
          </Button>
        {/each}
      </div>
    {/if}

    <form class="flex gap-2" onsubmit={submitFreeText}>
      <Input
        type="text"
        placeholder="Type an answer"
        bind:value={freeText}
        disabled={posting}
        aria-label="Answer"
      />
      <Button type="submit" size="sm" disabled={posting || !freeText.trim()}>Send</Button>
    </form>
  </Card.Content>
</Card.Root>
