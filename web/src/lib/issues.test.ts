import { describe, expect, it } from 'vitest'
import type { IssueView } from './api.gen'
import { toFlow } from './graph'
import {
  drawableOf,
  fixHash,
  issueMarkers,
  issueTarget,
  nextDraftIssues,
  withIssues,
  type DraftIssues,
} from './issues'
import { splitRoute } from './route'

const issue = (code: string, over: Partial<IssueView> = {}): IssueView => ({
  code,
  severity: 'error',
  message: `${code} message`,
  edges: [],
  ...over,
})

const drawable = { nodes: new Set(['a', 'b']), edges: new Set(['e:a>b#1', 'e:a>b#2']) }

describe('issue targets', () => {
  it('reads an edge issue first, all its edges, the first selected', () => {
    const t = issueTarget(issue('V40', { node: 'a', edges: ['e:a>b#2', 'e:a>b#1'] }), drawable)
    expect(t).toEqual({ kind: 'edge', edges: ['e:a>b#2', 'e:a>b#1'], first: 'e:a>b#2' })
  })

  it('reads a node issue when the node is drawn, else a document issue', () => {
    expect(issueTarget(issue('V10', { node: 'a' }), drawable)).toEqual({ kind: 'node', node: 'a' })
    expect(issueTarget(issue('V10', { node: 'gone' }), drawable)).toEqual({ kind: 'document', yaml: false })
    expect(issueTarget(issue('V01'), drawable)).toEqual({ kind: 'document', yaml: false })
    // Only targets present on the canvas are navigated to.
    expect(issueTarget(issue('V40', { edges: ['e:x>y#1'] }), drawable)).toEqual({ kind: 'document', yaml: false })
  })

  it('opens the YAML view for V06, which has nothing to draw', () => {
    const t = issueTarget(issue('V06', { node: 'a', edges: ['e:a>b#1'] }), drawable)
    expect(t).toEqual({ kind: 'document', yaml: true })
    expect(fixHash('ws', 'pb', t)).toBe('#/edit/ws/pb?yaml=1')
  })

  it('links "fix" to the node or the edge in the editor', () => {
    expect(fixHash('ws', 'pb', { kind: 'node', node: 'a b' })).toBe('#/edit/ws/pb?node=a%20b')
    expect(fixHash('ws', 'pb', { kind: 'edge', edges: ['id:x'], first: 'id:x' })).toBe('#/edit/ws/pb?edge=id%3Ax')
    expect(splitRoute('#/edit/ws/pb?edge=id%3Ax')).toEqual({ path: '#/edit/ws/pb', focus: { edge: 'id:x' } })
    expect(splitRoute('#/playbook/ws/pb?run=r1')).toEqual({ path: '#/playbook/ws/pb', focus: { run: 'r1' } })
    expect(splitRoute('#/playbook/ws/pb').focus).toEqual({})
  })
})

describe('issue markers', () => {
  it('marks nodes and every edge of an issue, joining two issues on one target', () => {
    const m = issueMarkers(
      [
        issue('V40', { edges: ['e:a>b#1', 'e:a>b#2'] }),
        issue('V50', { severity: 'warning', edges: ['e:a>b#1'] }),
        issue('V10', { severity: 'warning', node: 'b' }),
      ],
      drawable,
    )
    expect(m.edges.get('e:a>b#1')).toEqual({
      text: 'error V40: V40 message (edges e:a>b#1, e:a>b#2)\nwarning V50: V50 message (edges e:a>b#1)',
      error: true,
    })
    expect(m.edges.get('e:a>b#2')?.error).toBe(true)
    expect(m.nodes.get('b')).toEqual({ text: 'warning V10: V10 message (node b)', error: false })
  })

  it('sets markers on a flow and clears them when the issue is gone', () => {
    const pb = {
      id: 'p',
      name: 'p',
      nodes: [
        { id: 'a', type: 'start' },
        { id: 'b', type: 'finish' },
      ],
      edges: [{ from: 'a', to: 'b' }],
    } as never
    const flow = toFlow(pb)
    expect(drawableOf(flow.nodes, flow.edges).edges.has('e:a>b#1')).toBe(true)
    const marked = withIssues(flow.nodes, flow.edges, [issue('V40', { edges: ['e:a>b#1'] })], new Set(['e:a>b#1']))
    expect(marked.edges[0].data.issue?.error).toBe(true)
    expect(marked.edges[0].data.highlight).toBe(true)
    const cleared = withIssues(marked.nodes, marked.edges, [])
    expect(cleared.edges[0].data.issue).toBeUndefined()
    expect(cleared.edges[0].data.highlight).toBeUndefined()
  })
})

describe('draft validation', () => {
  it('keeps the last valid issues, marked stale, after a parse error', () => {
    const first: DraftIssues = { issues: [], stale: false, parseError: null }
    const valid = nextDraftIssues(first, { parse_error: null, issues: [issue('V10', { node: 'a' })] })
    expect(valid).toEqual({ issues: [issue('V10', { node: 'a' })], stale: false, parseError: null })
    const broken = nextDraftIssues(valid, { parse_error: 'bad indent', issues: [] })
    expect(broken).toEqual({ issues: valid.issues, stale: true, parseError: 'bad indent' })
    const fixed = nextDraftIssues(broken, { parse_error: null, issues: [] })
    expect(fixed.stale).toBe(false)
  })
})
