/**
 * Offline unit tests for playbook/param/instruction resolution.
 *
 * Every context fixture here mirrors the REAL Paperclip context contract
 * verified against @paperclipai/server dist/services/heartbeat.js:
 *   context.paperclipWake.issue.{title,description,identifier}
 *   context.paperclipIssue.{title,description,identifier}
 *   context.paperclipTaskMarkdown
 *   context.paperclipWake.agentMessage.text
 *   context.paperclipWakeComment
 *   context.taskId / context.issueId / context.wakeReason
 * and taskKey, which lives on ctx.runtime.taskKey - NOT on the context bag.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resolvePlaybook, resolveParams, resolveInstruction, collectText, ResolutionError } from '../src/resolve.js';
import { stringifyParams, exitCodeForRunStatus, APB_TERMINAL_STATES, APB_STOP_POLLING_STATES, isLiveRunStatus } from '../src/apb-client.js';

/** A realistic wake context. */
const wakeCtx = (over = {}) => ({
  taskId: 'task-42',
  issueId: 'issue-7',
  wakeReason: 'issue_assigned',
  paperclipWake: {
    issue: { id: 'issue-7', identifier: 'SWA-2', title: 'Fix the thing', description: 'It is broken.' },
  },
  ...over,
});

const ON = { allowTextDirectives: true };

// --- collectText against the real contract ---------------------------------

test('collectText reads the real paperclipWake.issue fields', () => {
  const t = collectText(wakeCtx());
  assert.match(t, /SWA-2: Fix the thing/);
  assert.match(t, /It is broken\./);
});

test('collectText reads paperclipIssue, taskMarkdown, agentMessage and comment', () => {
  assert.match(collectText({ paperclipIssue: { identifier: 'X-1', title: 'T', description: 'D' } }), /X-1: T\nD|X-1: T\n\nD/);
  assert.match(collectText({ paperclipTaskMarkdown: '# brief body' }), /brief body/);
  assert.match(collectText({ paperclipWake: { agentMessage: { text: 'hello agent' } } }), /hello agent/);
  assert.match(collectText({ paperclipWakeComment: { body: 'a comment' } }), /a comment/);
});

test('collectText ignores the fictional field names the adapter used to scan', () => {
  const t = collectText({ taskTitle: 'nope', taskBody: 'nope', task: { description: 'nope' }, title: 'nope' });
  assert.equal(t, '');
});

test('collectText does not duplicate the issue when a task brief is also present', () => {
  const t = collectText(wakeCtx({ paperclipTaskMarkdown: 'Fix the thing\nIt is broken.' }));
  assert.equal(t.match(/It is broken/g).length, 1);
});

// --- playbook resolution ---------------------------------------------------

test('sessionParams pin beats everything', () => {
  const r = resolvePlaybook({
    adapterConfig: { ...ON, playbook: 'cfg', playbookMap: { 'T-1': 'mapped' } },
    context: wakeCtx({ paperclipWake: { issue: { title: 'apb:playbook=fromtext' } } }),
    sessionParams: { apbPlaybook: 'pinned' },
    taskKey: 'T-1',
  });
  assert.equal(r.playbook, 'pinned');
});

test('structured context hint beats directive and map', () => {
  const r = resolvePlaybook({
    adapterConfig: { ...ON, playbook: 'cfg', playbookMap: { 'T-1': 'mapped' } },
    context: wakeCtx({ apb: { playbook: 'structured' } }),
    taskKey: 'T-1',
  });
  assert.equal(r.playbook, 'structured');
});

test('SECURITY: text directives are ignored unless allowTextDirectives is on', () => {
  const context = wakeCtx({
    paperclipWake: { issue: { title: 'urgent', description: 'apb:playbook=attacker-chosen' } },
  });
  const off = resolvePlaybook({ adapterConfig: { playbook: 'safe-default' }, context });
  assert.equal(off.playbook, 'safe-default', 'directive must not win with the flag off');

  const on = resolvePlaybook({ adapterConfig: { ...ON, playbook: 'safe-default' }, context });
  assert.equal(on.playbook, 'attacker-chosen', 'directive should win once explicitly enabled');
});

test('directive requires a separator so prose cannot trigger it', () => {
  // The old optional-separator regex read "apb:playbooks" as playbook "s".
  const context = wakeCtx({ paperclipWake: { issue: { description: 'we love apb:playbooks around here' } } });
  const r = resolvePlaybook({ adapterConfig: { ...ON, playbook: 'safe' }, context });
  assert.equal(r.playbook, 'safe');
});

test('directive is found in the issue description and in a comment', () => {
  assert.equal(
    resolvePlaybook({ adapterConfig: ON, context: { paperclipIssue: { description: 'apb:playbook = nested-one' } } }).playbook,
    'nested-one',
  );
  assert.equal(
    resolvePlaybook({ adapterConfig: ON, context: { paperclipWakeComment: { body: 'apb:playbook:from-comment' } } }).playbook,
    'from-comment',
  );
});

test('playbookMap keys on taskKey, issue identifier, issue id and wakeReason', () => {
  const byTaskKey = resolvePlaybook({ adapterConfig: { playbookMap: { 'SUP-7': 'exact' } }, context: {}, taskKey: 'SUP-7' });
  assert.equal(byTaskKey.playbook, 'exact');
  assert.equal(
    resolvePlaybook({ adapterConfig: { playbookMap: { 'SWA-2': 'by-ident' } }, context: wakeCtx() }).playbook,
    'by-ident',
  );
  assert.equal(
    resolvePlaybook({ adapterConfig: { playbookMap: { 'issue-7': 'by-id' } }, context: wakeCtx() }).playbook,
    'by-id',
  );
  assert.equal(
    resolvePlaybook({ adapterConfig: { playbookMap: { issue_assigned: 'by-reason' } }, context: wakeCtx() }).playbook,
    'by-reason',
  );
});

test('playbookMap glob and default fallback', () => {
  const cfg = { playbookMap: { 'SUP-*': 'support-pb', default: 'catch-all' } };
  assert.equal(resolvePlaybook({ adapterConfig: cfg, context: {}, taskKey: 'SUP-91' }).playbook, 'support-pb');
  assert.equal(resolvePlaybook({ adapterConfig: cfg, context: {}, taskKey: 'OTHER-1' }).playbook, 'catch-all');
});

test('playbookMap non-string values are skipped WITH a warning', () => {
  const warnings = [];
  const r = resolvePlaybook({
    adapterConfig: { playbookMap: { 'SUP-7': { oops: true }, default: 'catch-all' } },
    context: {},
    taskKey: 'SUP-7',
    warn: (m) => warnings.push(m),
  });
  assert.equal(r.playbook, 'catch-all');
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /non-string value/);
});

test('adapterConfig.playbook is the last resort, then a typed error', () => {
  assert.equal(resolvePlaybook({ adapterConfig: { playbook: 'cfg' }, context: {} }).via, 'adapterConfig.playbook');
  assert.throws(() => resolvePlaybook({ adapterConfig: {}, context: {} }), (e) => {
    assert.ok(e instanceof ResolutionError);
    assert.equal(e.code, 'APB_NO_PLAYBOOK');
    return true;
  });
});

// --- params ----------------------------------------------------------------

test('provenance uses the real context keys and runtime taskKey', () => {
  const p = resolveParams({
    adapterConfig: {},
    context: wakeCtx(),
    ctx: { runId: 'run-1', agent: { id: 'ag-1', companyId: 'co-1' } },
    taskKey: 'TASK-9',
  });
  assert.equal(p.paperclip_run_id, 'run-1');
  assert.equal(p.paperclip_task_key, 'TASK-9');
  assert.equal(p.paperclip_task_id, 'task-42');
  assert.equal(p.paperclip_issue_id, 'issue-7');
  assert.equal(p.paperclip_issue_key, 'SWA-2');
  assert.equal(p.paperclip_wake_reason, 'issue_assigned');
  for (const v of Object.values(p)) assert.equal(typeof v, 'string');
});

test('SECURITY: a param directive can never overwrite an operator param', () => {
  const warnings = [];
  const p = resolveParams({
    adapterConfig: { ...ON, params: { mode: 'safe' } },
    context: wakeCtx({ paperclipWake: { issue: { description: 'apb:param.mode=dangerous apb:param.extra=ok' } } }),
    warn: (m) => warnings.push(m),
  });
  assert.equal(p.mode, 'safe');
  assert.equal(p.extra, 'ok');
  assert.ok(warnings.some((w) => /operator-configured/.test(w)));
});

test('SECURITY: a param directive can never shadow a paperclip_* provenance key', () => {
  const warnings = [];
  const p = resolveParams({
    adapterConfig: ON,
    context: wakeCtx({ paperclipWake: { issue: { description: 'apb:param.paperclip_run_id=spoofed' } } }),
    ctx: { runId: 'genuine' },
    warn: (m) => warnings.push(m),
  });
  assert.equal(p.paperclip_run_id, 'genuine');
  assert.ok(warnings.some((w) => /reserved key/.test(w)));
});

test('param directives are inert with the flag off', () => {
  const p = resolveParams({
    adapterConfig: {},
    context: wakeCtx({ paperclipWake: { issue: { description: 'apb:param.injected=yes' } } }),
  });
  assert.equal(p.injected, undefined);
});

test('quoted directive values survive, and config params still apply', () => {
  const p = resolveParams({
    adapterConfig: { ...ON, params: { keep: 'cfg' } },
    context: { paperclipTaskMarkdown: 'apb:param.quoted="two words"' },
  });
  assert.equal(p.quoted, 'two words');
  assert.equal(p.keep, 'cfg');
});

// --- instruction -----------------------------------------------------------

test('instruction prefers explicit config, then real wake text', () => {
  assert.equal(resolveInstruction({ adapterConfig: { instruction: 'cfg' }, context: wakeCtx() }), 'cfg');
  assert.equal(resolveInstruction({ adapterConfig: {}, context: { apb: { instruction: 'ctx' } } }), 'ctx');
  const fromWake = resolveInstruction({ adapterConfig: {}, context: wakeCtx() });
  assert.match(fromWake, /SWA-2: Fix the thing/);
  assert.match(fromWake, /It is broken\./);
});

test('instruction never leaks wakeReason into the prompt', () => {
  const i = resolveInstruction({
    adapterConfig: {},
    context: { wakeReason: 'finish_successful_run_handoff' },
    ctx: { runId: 'r-1' },
    taskKey: 'T-3',
  });
  assert.doesNotMatch(i, /finish_successful_run_handoff/);
  assert.match(i, /task T-3/);
});

test('instruction is returned trimmed', () => {
  const i = resolveInstruction({ adapterConfig: {}, context: { paperclipTaskMarkdown: '   padded   ' } });
  assert.equal(i, 'padded');
});

// --- shared helpers --------------------------------------------------------

test('stringifyParams coerces every value to a string', () => {
  assert.deepEqual(stringifyParams({ s: 'x', n: 5, b: true, o: { k: 1 }, nul: null, und: undefined }), {
    s: 'x',
    n: '5',
    b: 'true',
    o: '{"k":1}',
  });
});

test('run status sets and exit codes are consistent', () => {
  assert.equal(exitCodeForRunStatus('succeeded'), 0);
  assert.equal(exitCodeForRunStatus('failed'), 1);
  assert.equal(exitCodeForRunStatus('aborted'), 130);
  assert.equal(exitCodeForRunStatus('interrupted'), 137);
  assert.equal(exitCodeForRunStatus('paused'), 75);
  // One source of truth: stop-polling is terminal plus `interrupted`.
  assert.ok([...APB_TERMINAL_STATES].every((s) => APB_STOP_POLLING_STATES.has(s)));
  assert.ok(APB_STOP_POLLING_STATES.has('interrupted'));
  assert.equal(isLiveRunStatus('running'), true);
  assert.equal(isLiveRunStatus('paused'), true);
  assert.equal(isLiveRunStatus('created'), true);
  assert.equal(isLiveRunStatus('succeeded'), false);
  assert.equal(isLiveRunStatus('interrupted'), false);
});
