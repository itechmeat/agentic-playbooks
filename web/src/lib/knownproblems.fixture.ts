// A known problem as the API returns it, for the known-problem tests.

import type { ProblemView } from './api.gen'

export function problem(over: Partial<ProblemView> = {}): ProblemView {
  return {
    id: 'kp-1',
    scope: { kind: 'path', name: 'scripts/build.sh' },
    symptom: 'the build fails on a deprecated flag',
    origin: { type: 'finding', kind: 'tool_problem' },
    evidence: [],
    runs: 2,
    first_seen_ms: 1,
    last_seen_ms: 2,
    status: 'open',
    significant: true,
    needs_look: false,
    source_now: 'unchanged',
    ...over,
  }
}
