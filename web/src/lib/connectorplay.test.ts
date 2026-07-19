import { describe, expect, it } from 'vitest'
import type { JsonSchema } from './connectors'
import {
  buildPlayFields,
  coerceFormValues,
  formatResultBody,
  isSimpleObjectSchema,
  parseRawArgs,
  resultSummary,
  type PlayField,
} from './connectorplay'

describe('isSimpleObjectSchema', () => {
  it('accepts an object schema whose properties are all simple leaves', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: {
        q: { type: 'string' },
        limit: { type: 'number' },
        active: { type: 'boolean' },
        kind: { type: 'string', enum: ['a', 'b'] },
      },
    }
    expect(isSimpleObjectSchema(schema)).toBe(true)
  })

  it('rejects a schema with a nested object property', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: { filter: { type: 'object' } },
    }
    expect(isSimpleObjectSchema(schema)).toBe(false)
  })

  it('rejects a schema with an array property', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: { tags: { type: 'array' } },
    }
    expect(isSimpleObjectSchema(schema)).toBe(false)
  })

  it('rejects a non-object top-level schema', () => {
    expect(isSimpleObjectSchema({ type: 'array' })).toBe(false)
  })

  it('rejects an object schema with no properties', () => {
    expect(isSimpleObjectSchema({ type: 'object' })).toBe(false)
  })

  it('rejects null and undefined', () => {
    expect(isSimpleObjectSchema(null)).toBe(false)
    expect(isSimpleObjectSchema(undefined)).toBe(false)
  })

  it('rejects a schema with a root-level oneOf', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: { q: { type: 'string' } },
      oneOf: [{ required: ['q'] }],
    }
    expect(isSimpleObjectSchema(schema)).toBe(false)
  })

  it('rejects a schema with a root-level anyOf', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: { q: { type: 'string' } },
      anyOf: [{ required: ['q'] }],
    }
    expect(isSimpleObjectSchema(schema)).toBe(false)
  })

  it('rejects a schema whose property carries oneOf', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: { id: { oneOf: [{ type: 'string' }, { type: 'number' }] } },
    }
    expect(isSimpleObjectSchema(schema)).toBe(false)
  })

  it('rejects a schema whose property carries anyOf', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: { id: { anyOf: [{ type: 'string' }, { type: 'number' }] } },
    }
    expect(isSimpleObjectSchema(schema)).toBe(false)
  })

  it('accepts a schema with an allOf conditional (e.g. github create_review)', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: {
        event: { type: 'string', enum: ['APPROVE', 'REQUEST_CHANGES', 'COMMENT'] },
        body: { type: 'string' },
      },
    }
    expect(isSimpleObjectSchema(schema)).toBe(true)
  })
})

describe('buildPlayFields', () => {
  it('returns one field per property, marking required ones', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: {
        q: { type: 'string', description: 'search text' },
        limit: { type: 'number' },
      },
      required: ['q'],
    }
    const fields = buildPlayFields(schema)
    expect(fields).toHaveLength(2)
    const q = fields.find((f) => f.name === 'q') as PlayField
    expect(q.kind).toBe('string')
    expect(q.required).toBe(true)
    expect(q.description).toBe('search text')
    const limit = fields.find((f) => f.name === 'limit') as PlayField
    expect(limit.kind).toBe('number')
    expect(limit.required).toBe(false)
  })

  it('maps a boolean property to the boolean kind', () => {
    const schema: JsonSchema = { type: 'object', properties: { active: { type: 'boolean' } } }
    expect(buildPlayFields(schema)[0].kind).toBe('boolean')
  })

  it('maps an enum property to the enum kind and carries its values', () => {
    const schema: JsonSchema = {
      type: 'object',
      properties: { kind: { type: 'string', enum: ['a', 'b'] } },
    }
    const [field] = buildPlayFields(schema)
    expect(field.kind).toBe('enum')
    expect(field.enumValues).toEqual(['a', 'b'])
  })

  it('returns an empty list for a schema with no properties', () => {
    expect(buildPlayFields({ type: 'object' })).toEqual([])
    expect(buildPlayFields(null)).toEqual([])
    expect(buildPlayFields(undefined)).toEqual([])
  })
})

describe('coerceFormValues', () => {
  const fields: PlayField[] = [
    { name: 'q', kind: 'string', required: false },
    { name: 'limit', kind: 'number', required: false },
    { name: 'active', kind: 'boolean', required: false },
  ]

  it('passes strings through unchanged', () => {
    expect(coerceFormValues(fields, { q: 'hello', limit: '', active: false })).toEqual({
      q: 'hello',
      active: false,
    })
  })

  it('parses a number field into a JS number', () => {
    expect(coerceFormValues(fields, { limit: '5', active: false })).toEqual({
      limit: 5,
      active: false,
    })
  })

  it('omits an empty optional field rather than sending an empty string', () => {
    const out = coerceFormValues(fields, { q: '', active: false })
    expect(out).not.toHaveProperty('q')
  })

  it('always includes a boolean field, defaulting to false', () => {
    expect(coerceFormValues(fields, {})).toEqual({ active: false })
  })

  it('drops an unparsable number rather than sending NaN', () => {
    const out = coerceFormValues(fields, { limit: 'not-a-number', active: false })
    expect(out).not.toHaveProperty('limit')
  })

  describe('enum fields', () => {
    const enumFields: PlayField[] = [
      { name: 'kind', kind: 'enum', required: false, enumValues: ['a', 'b'] },
      { name: 'priority', kind: 'enum', required: false, enumValues: [1, 2, 3] },
    ]

    it('coerces a numeric enum serialized as a string back to the original number', () => {
      const out = coerceFormValues(enumFields, { priority: '2' })
      expect(out.priority).toBe(2)
      expect(typeof out.priority).toBe('number')
    })

    it('passes through a matching string enum value unchanged', () => {
      expect(coerceFormValues(enumFields, { kind: 'b' })).toEqual({ kind: 'b' })
    })

    it('omits an unmatched enum value rather than sending it raw', () => {
      const out = coerceFormValues(enumFields, { kind: 'not-a-member' })
      expect(out).not.toHaveProperty('kind')
    })

    it('omits an empty enum value', () => {
      const out = coerceFormValues(enumFields, { kind: '' })
      expect(out).not.toHaveProperty('kind')
    })
  })
})

describe('parseRawArgs', () => {
  it('empty text parses to an empty object', () => {
    expect(parseRawArgs('')).toEqual({})
    expect(parseRawArgs('   ')).toEqual({})
  })

  it('parses a valid JSON object', () => {
    expect(parseRawArgs('{"q": "hi", "limit": 5}')).toEqual({ q: 'hi', limit: 5 })
  })

  it('throws a descriptive error on invalid JSON', () => {
    expect(() => parseRawArgs('{not json')).toThrow(/invalid JSON/)
  })

  it('throws on a non-object top level', () => {
    expect(() => parseRawArgs('[1, 2, 3]')).toThrow(/object/)
    expect(() => parseRawArgs('"just a string"')).toThrow(/object/)
    expect(() => parseRawArgs('42')).toThrow(/object/)
  })
})

describe('resultSummary', () => {
  it('summarizes a dry run as method and url', () => {
    expect(resultSummary({ ok: true, dry_run: true, method: 'GET', url: 'https://x/items' })).toBe(
      'dry run: GET https://x/items',
    )
  })

  it('summarizes a real success as status and "ok"', () => {
    expect(resultSummary({ ok: true, status: 200 })).toBe('200 ok')
  })

  it('summarizes an error by its code', () => {
    expect(resultSummary({ ok: false, error: { code: 'permission', message: 'nope' } })).toBe(
      'permission',
    )
  })

  it('includes the HTTP status on an error that carries one', () => {
    expect(
      resultSummary({
        ok: false,
        error: { code: 'auth', message: 'nope', http_status: 401 },
      }),
    ).toBe('auth (HTTP 401)')
  })
})

describe('formatResultBody', () => {
  it('pretty-prints a success body', () => {
    expect(formatResultBody({ ok: true, status: 200, body: { items: [] } })).toBe(
      JSON.stringify({ items: [] }, null, 2),
    )
  })

  it('pretty-prints the error object on failure', () => {
    const error = { code: 'permission', message: 'nope' }
    expect(formatResultBody({ ok: false, error })).toBe(JSON.stringify(error, null, 2))
  })

  it('pretty-prints the dry-run body', () => {
    expect(
      formatResultBody({ ok: true, dry_run: true, method: 'GET', url: 'https://x', body: null }),
    ).toBe(JSON.stringify(null, null, 2))
  })
})
