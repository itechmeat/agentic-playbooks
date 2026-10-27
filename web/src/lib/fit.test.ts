import { describe, expect, it } from 'vitest'
import type { FitReport } from './api.gen'
import {
  bySeverity,
  compatibilitySummary,
  consentSummary,
  fitBadges,
  itemLine,
  setupSummary,
  trustLine,
  trustSummary,
} from './fit'

const report = (over: Partial<FitReport> = {}): FitReport => ({
  id: 'p',
  scope: 'project',
  version: '1.0.0',
  ready: true,
  trusted: true,
  needs_consent: false,
  compatibility: [],
  setup: [],
  trust: [],
  consent: { irreversible: [], effects: [] },
  ...over,
})

describe('fitBadges', () => {
  it('keeps ready and trusted apart: an untrusted playbook can be ready', () => {
    const labels = fitBadges(report({ ready: true, trusted: false })).map((b) => b.label)
    expect(labels).toEqual(['ready', 'not trusted'])
  })
  it('marks a playbook that is not ready as a danger, an untrusted one as a warning', () => {
    const [ready, trusted] = fitBadges(report({ ready: false, trusted: false }))
    expect(ready).toEqual({ key: 'ready', label: 'not ready', tone: 'danger' })
    expect(trusted.tone).toBe('warn')
  })
  it('adds a consent badge only when a start must be agreed to', () => {
    expect(fitBadges(report()).map((b) => b.key)).not.toContain('consent')
    expect(fitBadges(report({ needs_consent: true })).map((b) => b.label)).toContain('needs consent')
  })
})

describe('section summaries', () => {
  it('reports blockers before notes', () => {
    const r = report({
      ready: false,
      compatibility: [
        { code: 'V76', subject: null, detail: 'glob matches nothing', severity: 'note' },
        { code: 'requires_unmet', subject: 'Cargo.toml', detail: 'missing', severity: 'blocking' },
      ],
    })
    expect(bySeverity(r.compatibility).blocking.map((i) => i.code)).toEqual(['requires_unmet'])
    expect(compatibilitySummary(r)).toBe('1 blocker: cannot run here')
    expect(compatibilitySummary(report())).toBe('compatible with this project')
  })
  it('counts what must be set up', () => {
    const r = report({
      setup: [
        { code: 'param_needs_value', subject: 'channel', detail: 'pass it', severity: 'blocking' },
        { code: 'connector_no_account', subject: 'gh', detail: 'add one', severity: 'blocking' },
      ],
    })
    expect(setupSummary(r)).toBe('2 things to set up first')
    expect(setupSummary(report())).toBe('nothing to set up')
  })
  it('tells a confirmation at start from an approval it needs', () => {
    const r = report({
      trusted: false,
      trust: [
        { kind: 'playbook', subject: 'p', digest: 'sha256:a', approved: false, acknowledge_covers: true },
        { kind: 'imported_scripts', subject: 'p', digest: 'sha256:a', approved: false, acknowledge_covers: false },
        { kind: 'profile_bundle', subject: 'project/dev', digest: null, approved: true, acknowledge_covers: true },
      ],
    })
    expect(trustSummary(r)).toBe(
      '2 parts not approved; 1 a confirmation at start covers; 1 needs an approval',
    )
    expect(trustLine(r.trust[1])).toBe('imported scripts p: not approved, needs an approval')
    expect(trustSummary(report())).toBe('every part is approved')
  })
  it('names the irreversible sources', () => {
    const r = report({ needs_consent: true, consent: { irreversible: ['node deploy'], effects: [] } })
    expect(consentSummary(r)).toBe('a start must be agreed to: node deploy')
    expect(consentSummary(report())).toBe('no irreversible effects')
  })
  it('summarizes sections with notes only', () => {
    const note = { code: 'V76', subject: null, detail: 'glob matches nothing', severity: 'note' as const }
    expect(compatibilitySummary(report({ compatibility: [note, note] }))).toBe('compatible, 2 notes')
    expect(setupSummary(report({ setup: [note] }))).toBe('nothing blocking, 1 note')
  })
  it('counts several approvals needed, and lines an approved part', () => {
    const hard = { kind: 'connector', subject: 'gh', digest: null, approved: false, acknowledge_covers: false }
    const r = report({ trust: [hard, { ...hard, subject: 'gl' }] })
    expect(trustSummary(r)).toBe('2 parts not approved; 2 need an approval')
    expect(trustLine({ ...hard, approved: true })).toBe('connector gh: approved')
  })
})

describe('itemLine', () => {
  it('leads with the subject when there is one', () => {
    expect(itemLine({ code: 'x', subject: 'dev', detail: 'missing', severity: 'blocking' })).toBe('dev: missing')
    expect(itemLine({ code: 'x', subject: null, detail: 'missing', severity: 'note' })).toBe('missing')
  })
})
