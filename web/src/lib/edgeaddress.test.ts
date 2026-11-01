import { describe, expect, it } from 'vitest'
import raw from '../../../crates/apb-core/tests/fixtures/edge_addresses.json?raw'
import {
  checkOrderText,
  duplicateEdgeId,
  edgeAddresses,
  encodeComponent,
  isValidEdgeId,
  parseAddress,
  renderAddress,
  resolveAddress,
  type AddressableEdge,
  type CheckOrder,
  type EdgeAddress,
} from './edgeaddress'
import { sha256Hex } from './sha256'

// The fixture the Rust module (`apb_core::edge_address`) reads too: parity
// with it is what keeps the two ports from drifting.
const fixture = JSON.parse(raw) as {
  cases: {
    name: string
    edges: AddressableEdge[]
    expected: { address: EdgeAddress; rendered: string; rank: number; check: CheckOrder }[]
  }[]
  strings: { address: EdgeAddress; rendered: string }[]
  invalid: string[]
}

describe('edge addresses (shared fixture)', () => {
  it('has cases to check', () => {
    expect(fixture.cases.length).toBeGreaterThan(0)
    expect(fixture.strings.length).toBeGreaterThan(0)
    expect(fixture.invalid.length).toBeGreaterThan(0)
  })

  for (const c of fixture.cases) {
    it(`derives every address, rank and check order: ${c.name}`, () => {
      const got = edgeAddresses(c.edges)
      expect(got).toEqual(c.expected.map((e, index) => ({ ...e, index })))
      for (const e of got) expect(resolveAddress(c.edges, e.address)).toBe(e.index)
      for (const e of got) expect(resolveAddress(c.edges, e.rendered)).toBe(e.index)
    })
  }

  it('renders and parses every string form', () => {
    for (const s of fixture.strings) {
      expect(renderAddress(s.address)).toBe(s.rendered)
      expect(parseAddress(s.rendered)).toEqual(s.address)
    }
  })

  it('refuses every invalid string', () => {
    for (const s of fixture.invalid) expect(parseAddress(s), s).toBeNull()
  })
})

describe('edge addresses (port details)', () => {
  it('keeps ends that look like separators apart', () => {
    expect(encodeComponent('a->b')).toBe('a-%3Eb')
    expect(encodeComponent('naïve')).toBe('na%C3%AFve')
    expect(parseAddress('e:na%C3%AFve>fin#1')).toEqual({ kind: 'derived', from: 'naïve', to: 'fin', ordinal: 1 })
  })

  it('refuses bytes that are not UTF-8 and a raw non-ASCII character', () => {
    expect(parseAddress('e:%FF>b#1')).toBeNull()
    expect(parseAddress('e:é>b#1')).toBeNull()
  })

  it('resolves an address that names no edge to -1', () => {
    expect(resolveAddress([{ from: 'a', to: 'b' }], 'e:a>b#2')).toBe(-1)
    expect(resolveAddress([{ from: 'a', to: 'b' }], 'not an address')).toBe(-1)
  })

  it('treats an explicit null condition as no condition', () => {
    expect(edgeAddresses([{ from: 'a', to: 'b', condition: null }])[0].check).toEqual({ kind: 'parallel' })
  })

  it('says the check order in words', () => {
    expect(checkOrderText({ kind: 'ordered', n: 2 })).toContain('2')
    expect(checkOrderText({ kind: 'fallback' })).toContain('fallback')
  })

  it('follows the edge id grammar', () => {
    expect(isValidEdgeId('0-retry_x')).toBe(true)
    expect(isValidEdgeId('-a')).toBe(false)
    expect(isValidEdgeId('A')).toBe(false)
    expect(isValidEdgeId('a'.repeat(64))).toBe(true)
    expect(isValidEdgeId('a'.repeat(65))).toBe(false)
  })
})

describe('sha256Hex', () => {
  it('matches the standard test vectors', () => {
    expect(sha256Hex('')).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855')
    expect(sha256Hex('abc')).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad')
    expect(sha256Hex('abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq')).toBe(
      '248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1',
    )
    expect(sha256Hex('a'.repeat(1000))).toBe('41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3')
  })
})

describe('duplicateEdgeId', () => {
  it('reduces each end to the id grammar and adds a hash suffix', () => {
    const id = duplicateEdgeId('Gate One', 'discard', 2, new Set())
    expect(id).toMatch(/^e-gate-one-discard-[0-9a-f]{6}$/)
    expect(id.endsWith(sha256Hex('Gate One\ndiscard\n2').slice(0, 6))).toBe(true)
    expect(isValidEdgeId(id)).toBe(true)
  })

  it('keeps at most 20 characters of each end and stays within 64', () => {
    const id = duplicateEdgeId('x'.repeat(40), 'y'.repeat(40), 1, new Set())
    expect(id.startsWith(`e-${'x'.repeat(20)}-${'y'.repeat(20)}-`)).toBe(true)
    expect(id.length).toBeLessThanOrEqual(64)
    expect(isValidEdgeId(id)).toBe(true)
  })

  it('takes a new suffix until the id is unused', () => {
    const first = duplicateEdgeId('a', 'b', 2, new Set())
    const second = duplicateEdgeId('a', 'b', 2, new Set([first]))
    expect(second).not.toBe(first)
    expect(second).toMatch(/^e-a-b-[0-9a-f]{6}$/)
  })
})
