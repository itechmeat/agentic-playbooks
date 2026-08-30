/**
 * LIVE integration tests - require a reachable apb engine.
 *
 * These fire REAL apb runs, but only ever against the throwaway `test-fixture`
 * project in this repo, whose single playbook (`apb-noop`) is one deterministic
 * shell script: no agent, no LLM, no connectors, no network. A business
 * playbook can never be reached from here.
 *
 * Run with `npm run test:live` (or `npm run test:all`). `npm test` runs the
 * offline suites only, so CI needs no engine.
 *
 * Orphan note: the timeout test deliberately abandons a run it started. That
 * run is the noop fixture, which finishes by itself in about a second, so it
 * leaves nothing behind that needs stopping.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execute, testEnvironment } from '../src/index.js';
import { ApbClient } from '../src/apb-client.js';

const APB = process.env.APB_BASE_URL ?? 'http://127.0.0.1:7321';
const PROJECT = 'test-fixture';
const PLAYBOOK = 'apb-noop';

/**
 * `skip` option predicates are evaluated at registration time, before any
 * before() hook runs, so the probe has to happen inside each test body.
 */
let apbUpPromise = null;
async function requireApb(t) {
  apbUpPromise ??= new ApbClient({ baseUrl: APB, requestTimeoutMs: 3000 })
    .health()
    .then(() => true)
    .catch(() => false);
  const up = await apbUpPromise;
  if (!up) t.skip(`apb not reachable at ${APB}`);
  return up;
}

/** A stand-in for the Paperclip host's AdapterExecutionContext. */
function makeCtx(config, { context = {}, runtime = {} } = {}) {
  const logs = [];
  const events = [];
  const metas = [];
  return {
    ctx: {
      runId: 'pc-run-test-1',
      agent: { id: 'agent-1', companyId: 'co-1', name: 'apb tester', adapterType: 'apb', adapterConfig: config },
      runtime: { sessionId: null, sessionParams: null, sessionDisplayId: null, taskKey: null, ...runtime },
      config,
      context,
      onLog: async (stream, chunk) => logs.push([stream, chunk]),
      onEvent: async (e) => events.push(e),
      onMeta: async (m) => metas.push(m),
    },
    logs,
    events,
    metas,
    text: () => logs.map(([, c]) => c).join(''),
  };
}

const BASE = { apbBaseUrl: APB, project: PROJECT, playbook: PLAYBOOK, timeoutMs: 120_000, pollIntervalMs: 500 };

test('testEnvironment passes against the live fixture project', async (t) => {
  if (!(await requireApb(t))) return;
  const r = await testEnvironment({ adapterType: 'apb', companyId: 'co-1', config: BASE });
  assert.ok(['pass', 'warn'].includes(r.status), `expected pass/warn, got ${r.status}: ${JSON.stringify(r.checks)}`);
  assert.ok(r.checks.some((c) => c.code === 'apb_health_ok'));
  assert.ok(r.checks.some((c) => c.code === 'apb_project_ok'));
  assert.ok(r.checks.some((c) => c.code === 'apb_playbook_ok'));
});

test('testEnvironment flags an unknown playbook', async (t) => {
  if (!(await requireApb(t))) return;
  const r = await testEnvironment({ adapterType: 'apb', config: { ...BASE, playbook: 'ghost' } });
  assert.equal(r.status, 'fail');
  assert.ok(r.checks.some((c) => c.code === 'apb_playbook_not_found'));
});

test('execute runs a real apb playbook end to end with a realistic wake context', async (t) => {
  if (!(await requireApb(t))) return;
  // The REAL context shape a Paperclip wake delivers.
  const { ctx, logs, events, metas, text } = makeCtx(BASE, {
    context: {
      taskId: 'task-42',
      issueId: 'issue-7',
      wakeReason: 'issue_assigned',
      paperclipWake: {
        issue: { id: 'issue-7', identifier: 'FIX-42', title: 'Run the fixture', description: 'Please run the noop.' },
      },
    },
    runtime: { taskKey: 'FIX-42' },
  });
  const r = await execute(ctx);

  assert.equal(r.exitCode, 0, `expected success, got ${JSON.stringify(r)}`);
  assert.equal(r.timedOut, false);
  assert.equal(r.signal, null);
  assert.equal(r.usageBasis, 'per_run');
  assert.equal(r.provider, 'apb');
  assert.equal(r.resultJson.apbRunStatus, 'succeeded');
  assert.equal(r.resultJson.apbPlaybook, PLAYBOOK);
  assert.match(r.sessionParams.apbRunId, /^apb-noop-\d+$/);
  assert.equal(r.sessionDisplayId, r.sessionParams.apbRunId);
  assert.match(r.summary, /apb-noop fixture OK/);

  const out = text();
  assert.match(out, /apb run started/);
  assert.match(out, /apb run finished: succeeded/);
  assert.ok(events.some((e) => e.eventType === 'apb.run_finished'));
  assert.ok(events.every((e) => e.eventType.length <= 120));
  assert.equal(metas.length, 1);
  assert.ok(logs.length > 0);

  // Provenance built from the REAL context keys reached apb.
  const detail = await new ApbClient({ baseUrl: APB }).getRun(
    r.sessionParams.apbRunId,
    r.sessionParams.apbWorkspaceId,
  );
  assert.equal(detail.params.paperclip_run_id, 'pc-run-test-1');
  assert.equal(detail.params.paperclip_task_key, 'FIX-42');
  assert.equal(detail.params.paperclip_task_id, 'task-42');
  assert.equal(detail.params.paperclip_issue_id, 'issue-7');
  assert.equal(detail.params.paperclip_issue_key, 'FIX-42');

  // And the instruction carried the REAL issue text, not a bare wakeReason.
  assert.match(detail.instruction ?? '', /FIX-42: Run the fixture/);
  assert.match(detail.instruction ?? '', /Please run the noop\./);
  assert.doesNotMatch(detail.instruction ?? '', /issue_assigned/);
});

test('an apb:playbook directive steers the run only when explicitly enabled', async (t) => {
  if (!(await requireApb(t))) return;
  const context = {
    paperclipWake: { issue: { title: 'please run', description: `apb:playbook=${PLAYBOOK}` } },
  };

  // Off (the default): the directive is ignored, so the bogus default is used.
  const off = await execute(makeCtx({ ...BASE, playbook: 'ghost-default' }, { context }).ctx);
  assert.equal(off.exitCode, 78);
  assert.equal(off.errorCode, 'APB_PLAYBOOK_NOT_FOUND');

  // On: the directive wins and a real run happens.
  const on = await execute(makeCtx({ ...BASE, playbook: 'ghost-default', allowTextDirectives: true }, { context }).ctx);
  assert.equal(on.exitCode, 0, `expected the directive to win, got ${JSON.stringify(on)}`);
  assert.equal(on.resultJson.apbPlaybook, PLAYBOOK);
});

test('execute times out cleanly and says the apb run continues', async (t) => {
  if (!(await requireApb(t))) return;
  const { ctx } = makeCtx({ ...BASE, timeoutMs: 1, pollIntervalMs: 250 });
  const r = await execute(ctx);
  assert.equal(r.timedOut, true);
  assert.equal(r.exitCode, null);
  assert.equal(r.errorCode, 'APB_RUN_TIMEOUT');
  assert.match(r.summary, /STILL RUNNING/);
  assert.equal(r.resultJson.stillLive, true);
  assert.ok(r.sessionParams.apbRunId, 'must hand back the run id so the next wake can re-attach');
});

test('a second wake re-attaches to the run the timeout left behind', async (t) => {
  if (!(await requireApb(t))) return;
  // Wake 1: give up almost immediately, leaving a live apb run.
  const first = await execute(makeCtx({ ...BASE, timeoutMs: 1 }).ctx);
  assert.equal(first.timedOut, true);
  const leftBehind = first.sessionParams.apbRunId;

  // Wake 2: carrying wake 1's sessionParams, it must adopt that run rather
  // than start a second one. This is the fix for the 3x amplification.
  const { ctx, events } = makeCtx(BASE, { runtime: { sessionParams: first.sessionParams } });
  const second = await execute(ctx);

  assert.equal(second.sessionParams.apbRunId, leftBehind, 'must be the SAME apb run, not a new one');
  assert.equal(second.exitCode, 0);
  // Either it re-attached to a still-live run, or the run had already finished
  // by the time wake 2 looked - both are correct, and neither starts a new run
  // while the old one is live.
  const reattached = events.some((e) => e.eventType === 'apb_run_reattached');
  assert.ok(reattached || second.resultJson.apbRunId === leftBehind);
});
