import { describe, expect, it } from 'vitest'
import { renderMarkdown } from './markdown'

describe('renderMarkdown blocks', () => {
  it('renders headings', () => {
    expect(renderMarkdown('## Account setup')).toContain('>Account setup</h2>')
    expect(renderMarkdown('## Account setup')).not.toContain('## ')
  })

  it('renders paragraphs and joins soft line breaks', () => {
    const html = renderMarkdown('one\ntwo\n\nthree')
    expect(html).toContain('one two')
    expect(html).toContain('three')
    expect(html.match(/<p /g)?.length).toBe(2)
  })

  it('renders inline code', () => {
    expect(renderMarkdown('set `api_base` first')).toContain('<code class="')
    expect(renderMarkdown('set `api_base` first')).not.toContain('`')
  })

  it('renders a fenced code block in its own horizontal scroller', () => {
    const html = renderMarkdown('```yaml\nkey: value\n```')
    expect(html).toContain('overflow-x-auto')
    expect(html).toContain('<code>key: value</code>')
    expect(html).not.toContain('```')
  })

  it('renders bullet and ordered lists', () => {
    expect(renderMarkdown('- one\n- two')).toContain('<ul')
    expect(renderMarkdown('- one\n- two')).toContain('<li>one</li><li>two</li>')
    expect(renderMarkdown('1. one\n2. two')).toContain('<ol')
  })

  it('renders links with a safe scheme', () => {
    const html = renderMarkdown('[docs](https://example.com/d)')
    expect(html).toContain('href="https://example.com/d"')
    expect(html).toContain('rel="noreferrer noopener"')
  })

  it('renders emphasis', () => {
    expect(renderMarkdown('**bold** and *italic*')).toContain('<strong>bold</strong>')
    expect(renderMarkdown('**bold** and *italic*')).toContain('<em>italic</em>')
  })
})

describe('renderMarkdown sanitising', () => {
  it('neutralises a script tag', () => {
    const html = renderMarkdown('# Hi\n\n<script>alert(1)</script>')
    expect(html).not.toContain('<script')
    expect(html).not.toContain('</script>')
    expect(html).toContain('&lt;script&gt;')
  })

  it('neutralises an onerror attribute', () => {
    const html = renderMarkdown('<img src=x onerror=alert(1)>')
    expect(html).not.toContain('<img')
    expect(html).toContain('&lt;img')
    // The text survives as visible content, but never as a live attribute:
    // it sits inside an escaped `&lt;img ...&gt;` string, not inside a tag.
    expect(/<[a-z]+[^>]*onerror/i.test(html)).toBe(false)
  })

  it('neutralises raw HTML inside a fenced code block', () => {
    const html = renderMarkdown('```html\n<script>alert(1)</script>\n```')
    expect(html).not.toContain('<script')
    expect(html).toContain('&lt;script&gt;')
  })

  it('drops a javascript: link but keeps its text', () => {
    const html = renderMarkdown('[click](javascript:alert(1))')
    expect(html).not.toContain('javascript:')
    expect(html).not.toContain('<a ')
    expect(html).toContain('click')
  })

  it('drops a data: link', () => {
    const html = renderMarkdown('[x](data:text/html,<script>alert(1)</script>)')
    expect(html).not.toContain('data:text/html')
    expect(html).not.toContain('<script')
  })

  it('escapes quotes so a link href cannot break out of its attribute', () => {
    const html = renderMarkdown('[x](https://e.com/"onmouseover="alert(1))')
    expect(html).not.toContain('onmouseover="alert')
    expect(html).toContain('&quot;')
  })

  it('escapes an ampersand once', () => {
    expect(renderMarkdown('a & b')).toContain('a &amp; b')
  })
})
