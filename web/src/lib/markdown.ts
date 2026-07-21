// A deliberately small Markdown to HTML renderer for connector documentation
// (`PUBLIC.md`), covering headings, paragraphs, lists, links, inline code and
// fenced code blocks.
//
// Security model: a connector folder can be installed from disk, so its
// documentation is untrusted input. Every character that is not produced by
// this file's own tags goes through `escapeHtml`, which means raw HTML in the
// source is never passed through: a `<script>` tag renders as visible text and
// an `onerror=` attribute can never attach to an element, because no attribute
// value in the output comes from the source except a link href, and that href
// is scheme-filtered. There is no allowlist of "safe" tags to get wrong.

const ESCAPES: Record<string, string> = {
  '&': '&amp;',
  '<': '&lt;',
  '>': '&gt;',
  '"': '&quot;',
  "'": '&#39;',
}

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ESCAPES[c] ?? c)
}

// Only schemes that cannot execute script are kept. Anything else (including
// `javascript:` and `data:`) renders as plain text instead of a link.
function safeUrl(raw: string): string | null {
  const u = raw.trim()
  if (u === '') return null
  if (u.startsWith('//')) return null
  if (/^(https?:|mailto:)/i.test(u)) return u
  if (/^[a-z][a-z0-9+.-]*:/i.test(u)) return null
  return u
}

const CODE_CLASS = 'rounded bg-muted px-1 py-0.5 font-mono text-[0.85em]'
const LINK_CLASS = 'text-primary hover:underline'

function inline(src: string): string {
  let out = ''
  let i = 0
  while (i < src.length) {
    const rest = src.slice(i)
    if (src[i] === '`') {
      const end = src.indexOf('`', i + 1)
      if (end > i) {
        out += `<code class="${CODE_CLASS}">${escapeHtml(src.slice(i + 1, end))}</code>`
        i = end + 1
        continue
      }
    }
    if (src[i] === '[') {
      const m = /^\[([^\]]*)\]\(([^)\s]*)\)/.exec(rest)
      if (m) {
        const url = safeUrl(m[2] ?? '')
        const text = inline(m[1] ?? '')
        out += url
          ? `<a href="${escapeHtml(url)}" target="_blank" rel="noreferrer noopener" class="${LINK_CLASS}">${text}</a>`
          : text
        i += m[0].length
        continue
      }
    }
    if (rest.startsWith('**')) {
      const end = src.indexOf('**', i + 2)
      if (end > i + 1) {
        out += `<strong>${inline(src.slice(i + 2, end))}</strong>`
        i = end + 2
        continue
      }
    }
    if (src[i] === '*' || src[i] === '_') {
      const end = src.indexOf(src[i] as string, i + 1)
      if (end > i + 1) {
        out += `<em>${inline(src.slice(i + 1, end))}</em>`
        i = end + 1
        continue
      }
    }
    out += escapeHtml(src[i] as string)
    i += 1
  }
  return out
}

const HEADING_CLASS: Record<number, string> = {
  1: 'mt-4 mb-2 text-base font-semibold first:mt-0',
  2: 'mt-4 mb-2 text-sm font-semibold first:mt-0',
  3: 'mt-3 mb-1.5 text-sm font-semibold first:mt-0',
  4: 'mt-3 mb-1.5 text-sm font-medium first:mt-0',
  5: 'mt-3 mb-1.5 text-xs font-medium first:mt-0',
  6: 'mt-3 mb-1.5 text-xs font-medium first:mt-0',
}

// Fenced code keeps its own horizontal scroller so a long line cannot push the
// page itself sideways on a phone.
const PRE_CLASS =
  'my-2 overflow-x-auto rounded-md border border-border bg-muted/40 p-3 font-mono text-xs first:mt-0 last:mb-0'
const P_CLASS = 'my-2 text-sm leading-relaxed first:mt-0 last:mb-0'
const LIST_CLASS = 'my-2 ml-5 text-sm leading-relaxed first:mt-0 last:mb-0'

const FENCE = /^(```|~~~)/
const HEADING = /^(#{1,6})\s+(.*)$/
const BULLET = /^\s*[-*+]\s+(.*)$/
const ORDERED = /^\s*\d+[.)]\s+(.*)$/
const RULE = /^\s*([-*_])\s*(?:\1\s*){2,}$/

export function renderMarkdown(src: string): string {
  const lines = src.replace(/\r\n?/g, '\n').split('\n')
  const out: string[] = []
  let i = 0

  while (i < lines.length) {
    const line = lines[i] ?? ''

    if (line.trim() === '') {
      i += 1
      continue
    }

    if (FENCE.test(line.trim())) {
      const fence = line.trim().slice(0, 3)
      const body: string[] = []
      i += 1
      while (i < lines.length && !(lines[i] ?? '').trim().startsWith(fence)) {
        body.push(lines[i] ?? '')
        i += 1
      }
      if (i < lines.length) i += 1 // consume the closing fence
      out.push(`<pre class="${PRE_CLASS}"><code>${escapeHtml(body.join('\n'))}</code></pre>`)
      continue
    }

    if (RULE.test(line)) {
      out.push('<hr class="my-3 border-border" />')
      i += 1
      continue
    }

    const heading = HEADING.exec(line)
    if (heading) {
      const level = (heading[1] ?? '#').length
      out.push(
        `<h${level} class="${HEADING_CLASS[level]}">${inline(heading[2] ?? '')}</h${level}>`,
      )
      i += 1
      continue
    }

    const listTag = BULLET.test(line) ? 'ul' : ORDERED.test(line) ? 'ol' : null
    if (listTag) {
      const pattern = listTag === 'ul' ? BULLET : ORDERED
      const items: string[] = []
      while (i < lines.length) {
        const m = pattern.exec(lines[i] ?? '')
        if (!m) break
        items.push(`<li>${inline(m[1] ?? '')}</li>`)
        i += 1
      }
      const marker = listTag === 'ul' ? 'list-disc' : 'list-decimal'
      out.push(`<${listTag} class="${LIST_CLASS} ${marker}">${items.join('')}</${listTag}>`)
      continue
    }

    // Paragraph: consecutive lines until a blank line or a line that starts a
    // different block. Soft line breaks collapse into spaces, as in Markdown.
    const para: string[] = []
    while (i < lines.length) {
      const l = lines[i] ?? ''
      if (
        l.trim() === '' ||
        FENCE.test(l.trim()) ||
        HEADING.test(l) ||
        BULLET.test(l) ||
        ORDERED.test(l) ||
        RULE.test(l)
      )
        break
      para.push(l.trim())
      i += 1
    }
    out.push(`<p class="${P_CLASS}">${inline(para.join(' '))}</p>`)
  }

  return out.join('')
}
