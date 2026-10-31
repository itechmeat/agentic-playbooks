<script lang="ts">
  // The apb mark (docs/brand/apb-logo.svg) inline, so its strokes follow the
  // theme through currentColor while the four dots keep the brand colors.
  // Decorative by default; pass `title` when the mark stands alone.
  let { class: className = 'size-5', title = '' }: { class?: string; title?: string } = $props()
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  viewBox="0 0 48 48"
  class={className}
  role={title ? 'img' : undefined}
  aria-hidden={title ? undefined : 'true'}
  aria-label={title || undefined}
>
  <path
    fill="none"
    stroke="currentColor"
    stroke-width="3.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    d="M11 8V40M20 8H40M20 18.7H33M20 29.3H37M20 40H29"
  />
  <circle cx="11" cy="8" r="3.6" fill="#7c5cff" />
  <circle cx="11" cy="18.7" r="3.6" fill="#22b8cf" />
  <circle cx="11" cy="29.3" r="3.6" fill="#f59e0b" />
  <circle cx="11" cy="40" r="3.6" fill="#10b981" />
</svg>
