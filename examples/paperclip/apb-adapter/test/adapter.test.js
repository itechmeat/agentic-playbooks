/**
 * Offline adapter tests. No apb engine required: every HTTP interaction is
 * driven through the client's injectable `fetchImpl`, so these exercise the
 * paths that are hard or destructive to reproduce live (SPA fallback, mid-body
 * reset, poll give-up, re-attach, pause).
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createServerAdapter,
  execute,
  testEnvironment,
  normalizeConfig,
  summarizeRun,
  isLoopbackHost,
  safeUrl,
  describeParams,
  sessionCodec,
} from '../src/index.js';
import { ApbClient, stripTrailingSlashes } from '../src/apb-client.js';

// ---------------------------------------------------------------------------
// fake transport
// ---------------------------------------------------------------------------

function res(body, { status = 200, contentType = 'application/json' } = {}) {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: { get: (h) => (h.toLowerCase() === 'content-type' ? contentType : null) },
    text: async () => (typeof body === 'string' ? body : JSON.stringify(body)),
  };
}

const SPA_HTML = '<!doctype html>\n<html><head><title>web</title></head><body></body></html>';

/**
 * Builds a fetchImpl from [matcher, responder] rules.
 * A matcher beginning with '=' is an EXACT pathname match; anything else is a
 * substring match on the whole URL. The exact form matters because
 * `/api/playbooks` is a prefix of `/api/playbooks/{id}/run`.
 */
function router(rules) {
  const calls = [];
  const impl = async (url, init = {}) => {
    calls.push({ url, method: init.method ?? 'GET', body: init.body ? JSON.parse(init.body) : null });
    const path = new URL(url).pathname;
    for (const [match, respond] of rules) {
      const hit = match.startsWith('=') ? path === match.slice(1) : url.includes(match);
      if (hit) return typeof respond === 'function' ? respond(calls.length, url, init) : respond;
    }
    // Anything unrouted behaves like apb's SPA fallback: 200 + index.html.
    return res(SPA_HTML, { contentType: 'application/octet-stream' });
  };
  impl.calls = calls;
  return impl;
}

const PROJECTS = [{ name: 'fix', path: '/tmp/fix', workspace_id: 'ws-1', playbook_count: 1 }];
const PLAYBOOKS = [{ id: 'pb', current: '1.0.0', workspace_id: 'ws-1', project: 'fix' }];

function runDetail(over = {}) {
  return {
    run_id: 'pb-1',
    run_status: 'succeeded',
    version: '1.0.0',
    answer: null,
    failure_reason: null,
    progress: {},
    nodes: { a: 'succeeded' },
    outputs: {},
    events: [{ seq: 0, type: 'run_started', playbook: 'pb', version: '1.0.0' }],
    ...over,
  };
}

/** A ctx with a fake transport wired into the adapter's client. */
function makeCtx(config, { context = {}, runtime = {} } = {}) {
  const logs = [];
  const events = [];
  return {
    ctx: {
      runId: 'pc-1',
      agent: { id: 'ag-1', companyId: 'co-1', name: 'a', adapterType: 'apb', adapterConfig: config },
      runtime: { sessionId: null, sessionParams: null, sessionDisplayId: null, taskKey: null, ...runtime },
      config,
      context,
      onLog: async (s, c) => logs.push([s, c]),
      onEvent: async (e) => events.push(e),
      onMeta: async () => {},
    },
    logs,
    events,
    text: () => logs.map(([, c]) => c).join(''),
  };
}

// The adapter constructs its own ApbClient, so tests that need a fake transport
// install it globally for the duration of the call.
async function withFetch(impl, fn) {
  const real = globalThis.fetch;
  globalThis.fetch = impl;
  try {
    return await fn();
  } finally {
    globalThis.fetch = real;
  }
}

const BASE = { apbBaseUrl: 'http://127.0.0.1:7321', project: 'fix', playbook: 'pb', pollIntervalMs: 250 };

// ---------------------------------------------------------------------------
// client-level guards
// ---------------------------------------------------------------------------

test('SPA fallback is rejected even when served as application/octet-stream', async () => {
  // apb 0.20.2 labels the static fallback application/octet-stream, not
  // text/html, so the doctype sniff is what has to catch it.
  const c = new ApbClient({ fetchImpl: async () => res(SPA_HTML, { contentType: 'application/octet-stream' }) });
  await assert.rejects(() => c.health(), (e) => e.code === 'APB_ROUTE_NOT_FOUND');
});

test('SPA fallback is rejected when served as text/html', async () => {
  const c = new ApbClient({ fetchImpl: async () => res(SPA_HTML, { contentType: 'text/html' }) });
  await assert.rejects(() => c.health(), (e) => e.code === 'APB_ROUTE_NOT_FOUND');
});

test('a valid JSON body with a sloppy content-type is still accepted', async () => {
  const c = new ApbClient({ fetchImpl: async () => res({ status: 'ok' }, { contentType: 'application/octet-stream' }) });
  assert.deepEqual(await c.health(), { status: 'ok' });
});

test('non-JSON garbage is reported as APB_BAD_RESPONSE', async () => {
  const c = new ApbClient({ fetchImpl: async () => res('not json at all', { contentType: 'application/json' }) });
  await assert.rejects(() => c.health(), (e) => e.code === 'APB_BAD_RESPONSE');
});

test('a mid-body connection reset classifies as APB_UNREACHABLE, not a raw TypeError', async () => {
  // Headers arrive, then the body read fails. The read must be inside the
  // error-mapping try or this surfaces as an unclassifiable TypeError.
  const impl = async () => ({
    ok: true,
    status: 200,
    headers: { get: () => 'application/json' },
    text: async () => {
      const err = new TypeError('terminated');
      err.cause = Object.assign(new Error('aborted'), { code: 'ECONNRESET' });
      throw err;
    },
  });
  const c = new ApbClient({ fetchImpl: impl });
  await assert.rejects(() => c.health(), (e) => {
    assert.equal(e.code, 'APB_UNREACHABLE');
    return true;
  });
});

test('a stalled body read hits the request timeout', async () => {
  const impl = async (_url, init) => ({
    ok: true,
    status: 200,
    headers: { get: () => 'application/json' },
    text: () =>
      new Promise((_resolve, reject) => {
        init.signal.addEventListener('abort', () => reject(Object.assign(new Error('aborted'), { name: 'AbortError' })));
      }),
  });
  const c = new ApbClient({ fetchImpl: impl, requestTimeoutMs: 30 });
  await assert.rejects(() => c.health(), (e) => e.code === 'APB_REQUEST_TIMEOUT');
});

test('trailing-slash trimming stays linear on a pathological baseUrl', () => {
  // Regression guard for CodeQL js/polynomial-redos. The old `/\/+$/` trim went
  // quadratic when a long slash run was followed by a non-slash: it began a
  // match at every offset in the run, consumed the rest, then failed `$`. The
  // shape below measured about six seconds at 80k under the old code and stays
  // near a millisecond now. baseUrl is config this package does not
  // length-check, so the bound has to come from the algorithm, not validation.
  const evil = `${'/'.repeat(80_000)}a`;

  const started = performance.now();
  const plain = new ApbClient({ baseUrl: `http://127.0.0.1:7321/${evil}` });
  const credentialed = new ApbClient({ baseUrl: `http://u:p@127.0.0.1:7321/${evil}` });
  const elapsed = performance.now() - started;

  // Nothing is trimmed: these do not end in a slash. The point is the cost.
  assert.ok(plain.baseUrl.endsWith('a'));
  assert.equal(credentialed.basicAuth, Buffer.from('u:p').toString('base64'));
  assert.ok(!credentialed.baseUrl.includes('u:p@'));
  assert.ok(elapsed < 1000, `construction took ${elapsed.toFixed(1)}ms`);

  // The trimming itself still has to be correct, including the userinfo path
  // that re-trims the rebuilt URL.
  assert.equal(new ApbClient({ baseUrl: 'http://127.0.0.1:7321///' }).baseUrl, 'http://127.0.0.1:7321');
  assert.equal(new ApbClient({ baseUrl: 'http://u:p@127.0.0.1:7321///' }).baseUrl, 'http://127.0.0.1:7321');
  assert.equal(stripTrailingSlashes('http://h/a/'), 'http://h/a');
  assert.equal(stripTrailingSlashes('http://h/a'), 'http://h/a');
  assert.equal(stripTrailingSlashes('/'.repeat(50_000)), '');
  assert.equal(stripTrailingSlashes(null), '');
});

test('failFromApbError arms: 404 / 409 / 429 map to distinct exit codes', async () => {
  const cases = [
    [404, 'not found', 78, 'APB_NOT_FOUND'],
    [409, JSON.stringify({ reason: 'untrusted_connector_requires_approve' }), 77, 'APB_PERMIT_REFUSED'],
    [429, 'workdir busy', 75, 'APB_WORKDIR_BUSY'],
  ];
  for (const [status, body, expectExit, expectCode] of cases) {
    const impl = router([
      ['/api/projects', res(PROJECTS)],
      ['=/api/playbooks', res(PLAYBOOKS)],
      ['/api/playbooks/pb/run', res(body, { status, contentType: 'application/json' })],
    ]);
    const { ctx } = makeCtx(BASE);
    const r = await withFetch(impl, () => execute(ctx));
    assert.equal(r.exitCode, expectExit, `status ${status}`);
    assert.equal(r.errorCode, expectCode);
    assert.ok(r.errorMessage, 'every failure must carry an errorMessage');
  }
});

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

test('project has NO default - a missing project is a config error', () => {
  assert.equal(normalizeConfig({}).project, null);
});

test('execute refuses to run without a project', async () => {
  const { ctx } = makeCtx({ playbook: 'pb' });
  const r = await execute(ctx);
  assert.equal(r.exitCode, 78);
  assert.equal(r.errorCode, 'APB_NO_PROJECT');
});

test('normalizeConfig parses JSON textareas, numbers and booleans', () => {
  const c = normalizeConfig({ playbookMap: '{"A-*":"x"}', params: '{"k":1}', timeoutMs: '5000', streamNodeOutput: 'false' });
  assert.deepEqual(c.playbookMap, { 'A-*': 'x' });
  assert.equal(c.timeoutMs, 5000);
  assert.equal(c.streamNodeOutput, false);
  assert.equal(normalizeConfig({ playbookMap: 'not json' }).playbookMap, null);
  // defaults
  assert.equal(normalizeConfig({}).allowTextDirectives, false);
  assert.equal(normalizeConfig({}).logParamValues, false);
});

test('asBool accepts 0/1 numbers as well as strings', () => {
  assert.equal(normalizeConfig({ streamNodeOutput: 0 }).streamNodeOutput, false);
  assert.equal(normalizeConfig({ allowTextDirectives: 1 }).allowTextDirectives, true);
  // A non-boolean-ish value keeps the default rather than coercing.
  assert.equal(normalizeConfig({ streamNodeOutput: 7 }).streamNodeOutput, true);
});

// ---------------------------------------------------------------------------
// logging hygiene
// ---------------------------------------------------------------------------

test('safeUrl strips userinfo', () => {
  assert.match(safeUrl('http://user:pw@apb.internal:7321/'), /userinfo redacted/);
  assert.doesNotMatch(safeUrl('http://user:pw@apb.internal:7321/'), /pw/);
  assert.equal(safeUrl('http://127.0.0.1:7321/'), 'http://127.0.0.1:7321/');
});

test('describeParams logs keys only by default and masks secretish keys when enabled', () => {
  const params = { customer_email: 'a@b.c', api_token: 'sekrit' };
  const hidden = describeParams(params, { logParamValues: false });
  assert.match(hidden, /customer_email/);
  assert.doesNotMatch(hidden, /a@b\.c/);
  const shown = describeParams(params, { logParamValues: true });
  assert.match(shown, /a@b\.c/);
  assert.doesNotMatch(shown, /sekrit/);
});

test('isLoopbackHost is not fooled by a prefix and covers mapped/any addresses', () => {
  assert.equal(isLoopbackHost('127.0.0.1'), true);
  assert.equal(isLoopbackHost('127.5.5.5'), true);
  assert.equal(isLoopbackHost('localhost'), true);
  assert.equal(isLoopbackHost('::1'), true);
  assert.equal(isLoopbackHost('::ffff:127.0.0.1'), true);
  assert.equal(isLoopbackHost('0.0.0.0'), true);
  assert.equal(isLoopbackHost('127.evil.com'), false, 'prefix match must not pass');
  assert.equal(isLoopbackHost('evil.com'), false);
  assert.equal(isLoopbackHost('10.0.0.1'), false);
});

test('the base URL is never logged with its userinfo intact', async () => {
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', res(runDetail({ events: [] }))],
  ]);
  const { ctx, text } = makeCtx({ ...BASE, apbBaseUrl: 'http://u:hunter2@127.0.0.1:7321' });
  await withFetch(impl, () => execute(ctx));
  assert.doesNotMatch(text(), /hunter2/);
});

test('param VALUES are not logged by default', async () => {
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', res(runDetail({ events: [] }))],
  ]);
  const { ctx, text } = makeCtx({ ...BASE, params: { customer: 'Ada Lovelace' } });
  await withFetch(impl, () => execute(ctx));
  assert.doesNotMatch(text(), /Ada Lovelace/);
  assert.match(text(), /customer/);
});

// ---------------------------------------------------------------------------
// poll loop behaviour
// ---------------------------------------------------------------------------

test('events are streamed once, seq-less events are not collapsed, nulls do not throw', async () => {
  const detail = runDetail({
    events: [
      { seq: 0, type: 'run_started', playbook: 'pb', version: '1' },
      null,
      { type: 'node_finished', node: 'x', status: 'succeeded', output: 'OUT-X' },
      { type: 'node_finished', node: 'y', status: 'succeeded', output: 'OUT-Y' },
      { seq: 3, type: 'run_finished', outcome: 'succeeded' },
    ],
  });
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', res(detail)],
  ]);
  const { ctx, text, events } = makeCtx(BASE);
  const r = await withFetch(impl, () => execute(ctx));
  assert.equal(r.exitCode, 0);
  // Both seq-less node_finished events survived (they would collapse onto the
  // key `undefined` if dedup keyed on ev.seq directly).
  assert.match(text(), /OUT-X/);
  assert.match(text(), /OUT-Y/);
  assert.equal(events.filter((e) => e.eventType === 'apb.node_finished').length, 2);
});

test('a 200 with an empty run body does not reject execute()', async () => {
  let n = 0;
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', () => (++n === 1 ? res('', { contentType: 'application/json' }) : res(runDetail()))],
  ]);
  const { ctx } = makeCtx(BASE);
  const r = await withFetch(impl, () => execute(ctx)); // must resolve, not throw
  assert.equal(r.exitCode, 0);
});

test('poll give-up is duration-based and reports the run as possibly live', async () => {
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', res('boom', { status: 500, contentType: 'text/plain' })],
  ]);
  const { ctx } = makeCtx({ ...BASE, pollIntervalMs: 250, pollGiveUpMs: 400, timeoutMs: 20_000 });
  const started = Date.now();
  const r = await withFetch(impl, () => execute(ctx));
  const elapsed = Date.now() - started;
  assert.ok(elapsed >= 400, `should have persisted for the give-up window, took ${elapsed}ms`);
  assert.equal(r.errorCode, 'APB_SERVER_ERROR');
  assert.equal(r.resultJson.stillLive, true);
  assert.equal(r.resultJson.apbRunId, 'pb-1');
  assert.ok(r.errorMessage.includes('MAY STILL BE LIVE'));
});

test('a transient poll blip does not abandon the run', async () => {
  let n = 0;
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', () => (++n <= 2 ? res('down', { status: 502, contentType: 'text/plain' }) : res(runDetail()))],
  ]);
  const { ctx } = makeCtx({ ...BASE, pollIntervalMs: 250, pollGiveUpMs: 60_000 });
  const r = await withFetch(impl, () => execute(ctx));
  assert.equal(r.exitCode, 0);
});

test('onPause=return yields exit 75 WITH an errorMessage and resultJson', async () => {
  const paused = runDetail({ run_status: 'paused', progress: { waiting_kind: 'human_review', waiting_on: 'gate' } });
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', res(paused)],
  ]);
  const { ctx } = makeCtx({ ...BASE, onPause: 'return' });
  const r = await withFetch(impl, () => execute(ctx));
  assert.equal(r.exitCode, 75);
  assert.equal(r.errorCode, 'APB_RUN_PAUSED');
  // Paperclip renders a generic "Adapter failed" without this.
  assert.match(r.errorMessage, /paused on human_review at node "gate"/);
  assert.equal(r.resultJson.apbRunStatus, 'paused');
  assert.equal(r.resultJson.stillLive, true);
  assert.equal(r.sessionParams.apbRunId, 'pb-1');
});

test('onPause=wait keeps polling until the run resolves', async () => {
  let n = 0;
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', () => (++n <= 2 ? res(runDetail({ run_status: 'paused' })) : res(runDetail()))],
  ]);
  const { ctx } = makeCtx({ ...BASE, onPause: 'wait', pollIntervalMs: 250 });
  const r = await withFetch(impl, () => execute(ctx));
  assert.equal(r.exitCode, 0);
});

// ---------------------------------------------------------------------------
// re-attach
// ---------------------------------------------------------------------------

test('a live prior run is re-attached instead of firing a second run', async () => {
  let n = 0;
  const impl = router([
    ['/api/playbooks/pb/run', res({ run_id: 'SHOULD-NOT-START' })],
    ['/api/runs/old-1', () =>
      ++n === 1 ? res(runDetail({ run_id: 'old-1', run_status: 'running' })) : res(runDetail({ run_id: 'old-1' }))],
  ]);
  const { ctx, events } = makeCtx(BASE, {
    runtime: { sessionParams: { apbRunId: 'old-1', apbWorkspaceId: 'ws-1', apbPlaybook: 'pb', apbProject: 'fix' } },
  });
  const r = await withFetch(impl, () => execute(ctx));

  assert.equal(r.exitCode, 0);
  assert.equal(r.sessionParams.apbRunId, 'old-1');
  assert.ok(events.some((e) => e.eventType === 'apb_run_reattached'), 'should emit a re-attach event');
  assert.ok(!impl.calls.some((c) => c.method === 'POST'), 'must not start a second apb run');
  // It also must not re-resolve the project/playbook: it already knows them.
  assert.ok(!impl.calls.some((c) => c.url.includes('/api/projects')));
});

test('a prior run that already finished leads to a fresh run', async () => {
  const impl = router([
    ['/api/runs/old-1', res(runDetail({ run_id: 'old-1', run_status: 'succeeded' }))],
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'new-1' })],
    ['/api/runs/new-1', res(runDetail({ run_id: 'new-1' }))],
  ]);
  const { ctx } = makeCtx(BASE, {
    runtime: { sessionParams: { apbRunId: 'old-1', apbWorkspaceId: 'ws-1', apbPlaybook: 'pb', apbProject: 'fix' } },
  });
  const r = await withFetch(impl, () => execute(ctx));
  assert.ok(impl.calls.some((c) => c.url.includes('/api/runs/old-1')), 'must probe the prior run first');
  assert.equal(r.sessionParams.apbRunId, 'new-1');
  assert.equal(r.exitCode, 0);
});

test('an unreachable prior run does not block a fresh start', async () => {
  const impl = router([
    ['/api/runs/gone-1', res('not found', { status: 404, contentType: 'text/plain' })],
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'new-1' })],
    ['/api/runs/new-1', res(runDetail({ run_id: 'new-1' }))],
  ]);
  const { ctx } = makeCtx(BASE, {
    runtime: { sessionParams: { apbRunId: 'gone-1', apbWorkspaceId: 'ws-1' } },
  });
  const r = await withFetch(impl, () => execute(ctx));
  assert.equal(r.exitCode, 0);
  assert.equal(r.sessionParams.apbRunId, 'new-1');
});

// ---------------------------------------------------------------------------
// summary + output suppression
// ---------------------------------------------------------------------------

test('summarizeRun picks the last node by EVENT order, not alphabetical key order', () => {
  // `outputs` is a serialized BTreeMap: alphabetical. The genuine last node
  // here is `finish-terminal`, which sorts in the middle.
  const detail = {
    run_status: 'succeeded',
    answer: null,
    outputs: { 'context-gate': '', 'finish-terminal': 'REAL ANSWER', 'track-gate': 'INTERMEDIATE' },
    events: [
      { seq: 1, type: 'node_finished', node: 'track-gate', output: 'INTERMEDIATE' },
      { seq: 2, type: 'node_finished', node: 'finish-terminal', output: 'REAL ANSWER' },
    ],
  };
  assert.equal(summarizeRun(detail, { playbook: 'p', runId: 'r' }), 'REAL ANSWER');
});

test('summarizeRun prefers answer, then failure_reason', () => {
  assert.equal(summarizeRun({ answer: 'A' }, { playbook: 'p', runId: 'r' }), 'A');
  assert.match(summarizeRun({ failure_reason: 'boom' }, { playbook: 'p', runId: 'r' }), /failed: boom/);
});

test('streamNodeOutput=false withholds output from logs, event payloads AND summary', async () => {
  const detail = runDetail({
    answer: null,
    events: [
      { seq: 0, type: 'run_started', playbook: 'pb', version: '1' },
      { seq: 1, type: 'node_finished', node: 'x', status: 'succeeded', output: 'SECRET-PAYLOAD' },
      { seq: 2, type: 'run_finished', outcome: 'succeeded' },
    ],
  });
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', res(detail)],
  ]);
  const { ctx, text, events } = makeCtx({ ...BASE, streamNodeOutput: false });
  const r = await withFetch(impl, () => execute(ctx));

  assert.doesNotMatch(text(), /SECRET-PAYLOAD/, 'log stream must not carry the output');
  const payloads = JSON.stringify(events);
  assert.doesNotMatch(payloads, /SECRET-PAYLOAD/, 'onEvent payloads must not carry the output');
  assert.doesNotMatch(JSON.stringify(r.resultJson), /SECRET-PAYLOAD/, 'resultJson must not carry the output');
  assert.doesNotMatch(r.summary, /SECRET-PAYLOAD/, 'summary must not lift the output');
});

test('streamNodeOutput=true does surface the output', async () => {
  const detail = runDetail({
    answer: null,
    events: [{ seq: 1, type: 'node_finished', node: 'x', status: 'succeeded', output: 'VISIBLE' }],
  });
  const impl = router([
    ['/api/projects', res(PROJECTS)],
    ['=/api/playbooks', res(PLAYBOOKS)],
    ['/api/playbooks/pb/run', res({ run_id: 'pb-1' })],
    ['/api/runs/', res(detail)],
  ]);
  const { ctx, text } = makeCtx({ ...BASE, streamNodeOutput: true });
  const r = await withFetch(impl, () => execute(ctx));
  assert.match(text(), /VISIBLE/);
  assert.equal(r.summary, 'VISIBLE');
});

// ---------------------------------------------------------------------------
// module contract + session codec
// ---------------------------------------------------------------------------

test('createServerAdapter satisfies the plugin-loader contract', () => {
  const m = createServerAdapter();
  assert.equal(m.type, 'apb');
  assert.equal(typeof m.execute, 'function');
  assert.equal(typeof m.testEnvironment, 'function');
  const schema = m.getConfigSchema();
  assert.ok(Array.isArray(schema.fields) && schema.fields.length > 0);
  for (const f of schema.fields) {
    assert.ok(f.key && f.label && f.type, `incomplete field ${JSON.stringify(f)}`);
    assert.ok(['text', 'select', 'toggle', 'number', 'textarea', 'combobox'].includes(f.type));
  }
  // project must be advertised as required now that it has no default.
  assert.equal(schema.fields.find((f) => f.key === 'project').required, true);
  assert.equal(schema.fields.find((f) => f.key === 'project').default, undefined);
});

test('sessionCodec preserves the server-owned __paperclip* metadata', () => {
  // heartbeat.js deserializes BEFORE stripping this metadata and re-attaches it
  // after serialize, so an allowlist that drops it breaks model-change reset.
  const raw = {
    apbRunId: 'r-1',
    __paperclipConfiguredModel: 'm',
    __paperclipConfigFingerprint: 'fp',
    junk: 'dropped',
  };
  const out = sessionCodec.deserialize(raw);
  assert.equal(out.apbRunId, 'r-1');
  assert.equal(out.__paperclipConfiguredModel, 'm');
  assert.equal(out.__paperclipConfigFingerprint, 'fp');
  assert.equal(out.junk, undefined);
  assert.deepEqual(sessionCodec.serialize(raw), out);
  assert.equal(sessionCodec.getDisplayId({ apbRunId: 'r-1' }), 'r-1');
  assert.equal(sessionCodec.deserialize({ nothing: 1 }), null);
  assert.equal(sessionCodec.deserialize(null), null);
});

// ---------------------------------------------------------------------------
// testEnvironment (offline arms)
// ---------------------------------------------------------------------------

test('testEnvironment fails without a project and with a bad URL', async () => {
  const noProject = await testEnvironment({ adapterType: 'apb', config: { apbBaseUrl: 'http://127.0.0.1:7321' } });
  assert.equal(noProject.status, 'fail');
  assert.ok(noProject.checks.some((c) => c.code === 'apb_project_missing'));

  const badUrl = await testEnvironment({ adapterType: 'apb', config: { apbBaseUrl: 'not-a-url', project: 'fix' } });
  assert.equal(badUrl.status, 'fail');
  assert.ok(badUrl.checks.some((c) => c.code === 'apb_base_url_invalid'));
});

test('testEnvironment warns when allowTextDirectives is enabled', async () => {
  const r = await testEnvironment({
    adapterType: 'apb',
    config: { apbBaseUrl: 'http://127.0.0.1:7999', project: 'fix', allowTextDirectives: true },
  });
  assert.ok(r.checks.some((c) => c.code === 'apb_text_directives_enabled' && c.level === 'warn'));
});

test('testEnvironment warns about credentials embedded in apbBaseUrl', async () => {
  const r = await testEnvironment({
    adapterType: 'apb',
    config: { apbBaseUrl: 'http://u:p@127.0.0.1:7999', project: 'fix' },
  });
  assert.ok(r.checks.some((c) => c.code === 'apb_base_url_userinfo'));
  assert.doesNotMatch(JSON.stringify(r.checks), /:p@/);
});
