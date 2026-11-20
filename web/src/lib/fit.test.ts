import { describe, expect, it } from 'vitest'
import type { FitReport } from './api.gen'
import {
  bySeverity,
  compatibilitySummary,
  consentSummary,
  fitBadges,
  itemLine,
  launchDetail,
  launchRecordLine,
  launchSummary,
  setupSummary,
  trustLine,
  trustSummary,
  probationLine,
  readinessLines,
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
  start_actions: [],
  launch: { policy: 'allow', sources: ['default'] },
  readiness: { ready: true, items: [], probation: undefined },
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

describe('launch', () => {
  it('says what an agent may do under each policy', () => {
    expect(launchSummary(report().launch)).toBe('an agent may launch it')
    expect(launchSummary({ policy: 'ask', sources: [] })).toContain('after a person confirms')
    expect(launchSummary({ policy: 'deny', sources: [] })).toBe('only a person launches it')
  })
  it('puts a problem before the policy', () => {
    const l = { policy: 'deny' as const, sources: [], problem: 'policy_file_invalid: bad key' }
    expect(launchSummary(l)).toContain('refused until this is fixed')
  })
  it('names the sources and the floor', () => {
    const l = { policy: 'ask' as const, sources: ['launch floor of `p`'], floor: 'ask' as const }
    expect(launchDetail(l)).toBe('from launch floor of `p`; launch floor ask')
    expect(launchDetail({ policy: 'allow', sources: [] })).toBe('no launch floor')
  })
  it('reads a run record without a nonce', () => {
    const line = launchRecordLine({
      kind: 'resume_all',
      initiator: 'agent_mcp',
      confirmation: 'host_relayed',
      policy: 'ask',
      sources: ['machine policy.yaml launch.agent'],
    })
    expect(line).toBe(
      'resume all by agent mcp, confirmed by host relayed; policy ask (machine policy.yaml launch.agent)',
    )
  })
})

describe('readinessLines', () => {
  const base: FitReport = {
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
    start_actions: [],
    launch: { policy: 'allow', sources: [] },
    readiness: {
      ready: false,
      items: [
        { code: 'done_defined', ok: true, detail: 'the goal has 1 enforced script criteria' },
        { code: 'checked_actors', ok: true, detail: 'every acting agent_task has a deterministic check' },
        { code: 'budget', ok: false, detail: 'no budget: a run alone spends without a limit' },
      ],
      probation: undefined,
    },
  }

  it('lists what is missing', () => {
    const { lines, okCount, ready } = readinessLines(base)
    expect(lines).toEqual(['no budget: a run alone spends without a limit'])
    expect(okCount).toBe(2)
    expect(ready).toBe(false)
  })

  it('collapses the ok items', () => {
    const all: FitReport = {
      ...base,
      readiness: {
        ...base.readiness,
        ready: true,
        items: base.readiness.items.map((i) => ({ ...i, ok: true })),
      },
    }
    const { lines, okCount, ready } = readinessLines(all)
    expect(lines).toEqual([])
    expect(okCount).toBe(3)
    expect(ready).toBe(true)
  })
})

describe('probationLine', () => {
  const p = { required: 2, counted: 1, source: 'playbook', waived: false, ledger_ok: true }
  it('counts attended runs and claims no person', () => {
    expect(probationLine(p)).toBe('probation: 1 of 2 attended runs counted in this project')
    expect(probationLine(p)).not.toMatch(/person/)
  })
  it('says a machine waiver, also over a ledger that does not verify', () => {
    expect(probationLine({ ...p, waived: true })).toBe(
      'probation: waived on this machine (1 of 2 attended runs counted in this project)',
    )
    expect(probationLine({ ...p, waived: true, ledger_ok: false, counted: 0 })).toBe(
      'probation: waived on this machine (0 of 2 attended runs counted in this project, its ledger does not verify)',
    )
  })
  it('names a ledger that does not verify', () => {
    expect(probationLine({ ...p, ledger_ok: false, counted: 0 })).toBe(
      'probation: the ledger of this playbook does not verify, so nothing counts until it is moved aside',
    )
  })
  it('is carried by readinessLines when the playbook asks for one', () => {
    const r = report({ readiness: { ready: false, items: [], probation: p } })
    expect(readinessLines(r).probation).toBe('probation: 1 of 2 attended runs counted in this project')
    expect(readinessLines(report()).probation).toBeNull()
  })
})
