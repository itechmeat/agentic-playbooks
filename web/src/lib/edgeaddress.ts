// The address of one edge inside one playbook definition: the dashboard's
// port of `crates/apb-core/src/edge_address.rs`. Both read
// `crates/apb-core/tests/fixtures/edge_addresses.json` in their tests, so the
// two cannot drift.
//
// An edge has no identity of its own unless its author gave it an `id`, and
// two edges may share both ends. An address names exactly one edge of one
// definition: `id:<id>` for an edge with an authored id, else
// `e:<from>><to>#<ordinal>`, where the ordinal is the 1-based position among
// the id-less edges with exactly those ends, in declaration order. The ends
// are percent-encoded outside `[A-Za-z0-9_.-]`, so parsing is exact: equal
// strings mean equal addresses.
//
// An address is not an identity across versions; the version diff's
// correspondence says which edges of two versions are the same.

import { sha256Hex } from './sha256'

// The wire shapes of `apb_core::edge_address`, the issue view and the diff
// correspondence, generated from the Rust types.
import type {
  AddressedEdge,
  CheckOrder,
  EdgeAddress,
  EdgeCorrespondence,
  EdgeOrderChange,
  IssueView,
} from './api.gen'

export type { AddressedEdge, CheckOrder, EdgeAddress, EdgeCorrespondence, EdgeOrderChange, IssueView }

/** The longest authored edge id. */
export const MAX_EDGE_ID_LEN = 64

/** Whether `id` is a well-formed authored edge id: `[a-z0-9][a-z0-9_-]*`, at most 64 characters. */
export function isValidEdgeId(id: string): boolean {
  return id.length <= MAX_EDGE_ID_LEN && /^[a-z0-9][a-z0-9_-]*$/.test(id)
}

/** The fields of an edge an address and a check order read. */
export interface AddressableEdge {
  id?: unknown
  from: string
  to: string
  condition?: unknown
  fallback?: unknown
}

function isPlain(b: number): boolean {
  return (
    (b >= 0x30 && b <= 0x39) ||
    (b >= 0x41 && b <= 0x5a) ||
    (b >= 0x61 && b <= 0x7a) ||
    b === 0x5f ||
    b === 0x2e ||
    b === 0x2d
  )
}

/** Percent-encodes every UTF-8 byte outside `[A-Za-z0-9_.-]` as `%XX` (upper-case hex). */
export function encodeComponent(s: string): string {
  let out = ''
  for (const b of new TextEncoder().encode(s)) {
    out += isPlain(b) ? String.fromCharCode(b) : `%${b.toString(16).toUpperCase().padStart(2, '0')}`
  }
  return out
}

/**
 * The exact inverse of `encodeComponent`: an encoded plain byte, lower-case
 * hex, a stray `%`, a byte that must be encoded or bytes that are not UTF-8
 * make the component invalid (null), so one address has one string form.
 */
function decodeComponent(s: string): string | null {
  const bytes: number[] = []
  let i = 0
  while (i < s.length) {
    const c = s.charCodeAt(i)
    if (c === 0x25) {
      const hex = s.slice(i + 1, i + 3)
      if (!/^[0-9A-F]{2}$/.test(hex)) return null
      const v = parseInt(hex, 16)
      if (isPlain(v)) return null
      bytes.push(v)
      i += 3
    } else if (c < 0x80 && isPlain(c)) {
      bytes.push(c)
      i += 1
    } else {
      return null
    }
  }
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(bytes))
  } catch {
    return null
  }
}

/** The string form: `id:<id>` or `e:<from>><to>#<ordinal>`. */
export function renderAddress(a: EdgeAddress): string {
  if (a.kind === 'authored') return `id:${encodeComponent(a.id)}`
  return `e:${encodeComponent(a.from)}>${encodeComponent(a.to)}#${a.ordinal}`
}

/**
 * Parses the string form exactly, or returns null: anything `renderAddress`
 * would not produce is refused.
 */
export function parseAddress(s: string): EdgeAddress | null {
  if (s.startsWith('id:')) {
    const id = decodeComponent(s.slice(3))
    return id ? { kind: 'authored', id } : null
  }
  if (!s.startsWith('e:')) return null
  const rest = s.slice(2)
  const hash = rest.lastIndexOf('#')
  if (hash < 0) return null
  const ends = rest.slice(0, hash)
  const ordinal = rest.slice(hash + 1)
  const gt = ends.indexOf('>')
  if (gt < 0) return null
  if (!/^[1-9][0-9]*$/.test(ordinal)) return null
  const n = Number(ordinal)
  if (!Number.isSafeInteger(n) || n > 0xffffffff) return null
  const from = decodeComponent(ends.slice(0, gt))
  const to = decodeComponent(ends.slice(gt + 1))
  if (!from || !to) return null
  return { kind: 'derived', from, to, ordinal: n }
}

/** Whether two addresses name the same edge of one definition. */
export function sameAddress(a: EdgeAddress, b: EdgeAddress): boolean {
  return renderAddress(a) === renderAddress(b)
}

function hasCondition(e: AddressableEdge): boolean {
  return e.condition !== undefined && e.condition !== null
}

/** The address of every edge, aligned with `edges`. */
export function edgeAddresses(edges: readonly AddressableEdge[]): AddressedEdge[] {
  const ordinals = new Map<string, number>()
  const ranks = new Map<string, number>()
  const conditional = new Map<string, number>()
  return edges.map((e, index) => {
    let address: EdgeAddress
    if (typeof e.id === 'string') {
      address = { kind: 'authored', id: e.id }
    } else {
      const key = `${e.from}\n${e.to}`
      const n = (ordinals.get(key) ?? 0) + 1
      ordinals.set(key, n)
      address = { kind: 'derived', from: e.from, to: e.to, ordinal: n }
    }
    const rank = ranks.get(e.from) ?? 0
    ranks.set(e.from, rank + 1)
    const before = conditional.get(e.from) ?? 0
    let check: CheckOrder
    if (e.fallback === true) {
      check = { kind: 'fallback' }
    } else if (hasCondition(e)) {
      check = { kind: 'ordered', n: before + 1 }
      conditional.set(e.from, before + 1)
    } else {
      check = { kind: 'parallel' }
    }
    return { address, rendered: renderAddress(address), index, rank, check }
  })
}

/**
 * The index in `edges` of the edge `address` names (an address or its string
 * form), or -1. An id used twice (a validation error) resolves to its first
 * edge.
 */
export function resolveAddress(edges: readonly AddressableEdge[], address: EdgeAddress | string): number {
  const rendered = typeof address === 'string' ? address : renderAddress(address)
  return edgeAddresses(edges).find((a) => a.rendered === rendered)?.index ?? -1
}

/** The check order in words, for the edge panel. */
export function checkOrderText(check: CheckOrder): string {
  if (check.kind === 'parallel') return 'always taken, with the other unconditional edges'
  if (check.kind === 'fallback') return 'fallback: taken when no other condition matched'
  return `condition checked as number ${check.n} of its source's conditions`
}

/** Up to 20 characters of an end, reduced to the edge id grammar. */
function idPart(end: string): string {
  return Array.from(end)
    .slice(0, 20)
    .map((ch) => {
      if (/^[a-z0-9_-]$/.test(ch)) return ch
      if (/^[A-Z]$/.test(ch)) return ch.toLowerCase()
      return '-'
    })
    .join('')
}

/**
 * An id for a new edge that would otherwise be an id-less duplicate of an
 * existing `from`/`to` pair: `e-<from>-<to>-<6 hex>`, the hex taken from the
 * sha256 of `from`, `to` and the ordinal it would have had (then of the same
 * with an increasing counter until the id is not in `taken`).
 */
export function duplicateEdgeId(from: string, to: string, ordinal: number, taken: ReadonlySet<string>): string {
  const base = `e-${idPart(from)}-${idPart(to)}`
  for (let counter = 0; ; counter++) {
    const seed = `${from}\n${to}\n${ordinal}${counter ? `\n${counter}` : ''}`
    const id = `${base}-${sha256Hex(seed).slice(0, 6)}`.slice(0, MAX_EDGE_ID_LEN)
    if (!taken.has(id)) return id
  }
}
