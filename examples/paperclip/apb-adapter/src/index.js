/**
 * Paperclip external server adapter: `apb`.
 *
 * Dispatches Paperclip agent work to a locally running apb (agentic-playbooks)
 * engine over its HTTP API, streams the apb run journal back into the Paperclip
 * run log, and maps the final apb run state onto a Paperclip execution result.
 *
 * Contract source of truth: @paperclipai/adapter-utils `dist/types.d.ts`
 * (ServerAdapterModule / AdapterExecutionContext / AdapterExecutionResult),
 * validated by @paperclipai/server `dist/adapters/plugin-loader.js`.
 */

import {
  ApbClient,
  ApbError,
  APB_STOP_POLLING_STATES,
  isLiveRunStatus,
  exitCodeForRunStatus,
} from './apb-client.js';
import { resolvePlaybook, resolveParams, resolveInstruction, ResolutionError } from './resolve.js';
import {
  getConfigSchema,
  DEFAULT_APB_BASE_URL,
  DEFAULT_TIMEOUT_MS,
  DEFAULT_POLL_INTERVAL_MS,
  DEFAULT_POLL_GIVE_UP_MS,
  MIN_POLL_INTERVAL_MS,
} from './config-schema.js';

export const type = 'apb';
export const label = 'Agentic Playbooks (apb)';

// ---------------------------------------------------------------------------
// config coercion
// ---------------------------------------------------------------------------

function asString(v, fallback = '') {
  return typeof v === 'string' && v.trim() ? v.trim() : fallback;
}

function asNumber(v, fallback) {
  const n = typeof v === 'number' ? v : typeof v === 'string' && v.trim() ? Number(v) : NaN;
  return Number.isFinite(n) && n > 0 ? n : fallback;
}

function asBool(v, fallback) {
  if (typeof v === 'boolean') return v;
  if (typeof v === 'number') {
    if (v === 1) return true;
    if (v === 0) return false;
  }
  if (typeof v === 'string') {
    if (/^(true|yes|1|on)$/i.test(v.trim())) return true;
    if (/^(false|no|0|off)$/i.test(v.trim())) return false;
  }
  return fallback;
}

/** Config textareas arrive as JSON strings from the UI, or as real objects from the API. */
function asObject(v) {
  if (!v) return null;
  if (typeof v === 'object' && !Array.isArray(v)) return v;
  if (typeof v === 'string' && v.trim()) {
    try {
      const parsed = JSON.parse(v);
      return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : null;
    } catch {
      return null;
    }
  }
  return null;
}

export function normalizeConfig(raw = {}) {
  const cfg = raw && typeof raw === 'object' ? raw : {};
  return {
    apbBaseUrl: asString(cfg.apbBaseUrl ?? cfg.baseUrl ?? cfg.url, DEFAULT_APB_BASE_URL),
    apbApiKey: asString(cfg.apbApiKey ?? cfg.apiKey, '') || null,
    // No default: see config-schema.js. An unset project is a config error.
    project: asString(cfg.project, '') || null,
    playbook: asString(cfg.playbook, '') || null,
    playbookMap: asObject(cfg.playbookMap),
    params: asObject(cfg.params),
    instruction: asString(cfg.instruction, '') || null,
    timeoutMs: asNumber(cfg.timeoutMs, DEFAULT_TIMEOUT_MS),
    pollIntervalMs: Math.max(MIN_POLL_INTERVAL_MS, asNumber(cfg.pollIntervalMs, DEFAULT_POLL_INTERVAL_MS)),
    pollGiveUpMs: asNumber(cfg.pollGiveUpMs, DEFAULT_POLL_GIVE_UP_MS),
    onPause: cfg.onPause === 'wait' ? 'wait' : 'return',
    streamNodeOutput: asBool(cfg.streamNodeOutput, true),
    allowTextDirectives: asBool(cfg.allowTextDirectives, false),
    logParamValues: asBool(cfg.logParamValues, false),
  };
}

// ---------------------------------------------------------------------------
// logging hygiene
// ---------------------------------------------------------------------------

const SECRETISH_KEY = /(token|key|secret|password|passwd|credential|auth)/i;

/** A URL safe to log: userinfo (user:pass@) is removed, never echoed. */
export function safeUrl(raw) {
  try {
    const u = new URL(raw);
    if (u.username || u.password) {
      u.username = '';
      u.password = '';
      return `${u.toString()} (userinfo redacted)`;
    }
    return u.toString();
  } catch {
    return '<unparseable url>';
  }
}

/**
 * Param map rendered for the log. Keys only by default: apb run params routinely
 * carry customer identifiers lifted from the issue.
 */
export function describeParams(params, { logParamValues }) {
  const keys = Object.keys(params);
  if (!keys.length) return 'none';
  if (!logParamValues) return `${keys.length} param(s): ${keys.sort().join(', ')} (values hidden; set logParamValues to show)`;
  const shown = {};
  for (const k of keys.sort()) shown[k] = SECRETISH_KEY.test(k) ? '***' : params[k];
  return JSON.stringify(shown);
}

/** Proper loopback test - a prefix match would accept `127.evil.com`. */
export function isLoopbackHost(hostname) {
  if (!hostname) return false;
  const h = hostname.toLowerCase().replace(/^\[|\]$/g, '');
  if (h === 'localhost' || h.endsWith('.localhost')) return true;
  if (h === '::1' || h === '::') return true;
  // IPv4-mapped IPv6, e.g. ::ffff:127.0.0.1
  const mapped = /^::ffff:(\d{1,3}(?:\.\d{1,3}){3})$/i.exec(h);
  const v4 = mapped ? mapped[1] : h;
  const octets = v4.split('.');
  if (octets.length !== 4) return false;
  const nums = octets.map((o) => (/^\d{1,3}$/.test(o) ? Number(o) : NaN));
  if (nums.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) return false;
  // 127.0.0.0/8 is loopback; 0.0.0.0 is "this host" and equally local.
  return nums[0] === 127 || (nums[0] === 0 && nums[1] === 0 && nums[2] === 0 && nums[3] === 0);
}

// ---------------------------------------------------------------------------
// log / event streaming
// ---------------------------------------------------------------------------

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Never let a host callback failure kill the run. */
async function safe(fn, ...args) {
  if (typeof fn !== 'function') return;
  try {
    await fn(...args);
  } catch {
    /* the host's sink is best-effort from our side */
  }
}

function truncate(s, max = 4000) {
  const str = String(s ?? '');
  return str.length > max ? `${str.slice(0, max)}\n… [truncated ${str.length - max} chars]` : str;
}

/**
 * Strip node output from an event before it leaves the adapter.
 * `streamNodeOutput:false` must govern every egress path, not just the
 * rendered log line: the raw event is also handed to onEvent as `payload`.
 */
function scrubEvent(ev, { streamNodeOutput }) {
  if (streamNodeOutput) return ev;
  if (!ev || typeof ev !== 'object') return ev;
  if (!('output' in ev) && !('summary' in ev) && !('question' in ev)) return ev;
  const copy = { ...ev };
  if ('output' in copy) copy.output = '[withheld: streamNodeOutput=false]';
  if ('summary' in copy) copy.summary = '[withheld: streamNodeOutput=false]';
  if ('question' in copy) copy.question = '[withheld: streamNodeOutput=false]';
  return copy;
}

/** One line of human-readable text per apb journal event. */
function renderEvent(ev, { streamNodeOutput }) {
  const t = ev.type;
  const node = ev.node ? ` node=${ev.node}` : '';
  switch (t) {
    case 'run_started':
      return `apb run started (playbook ${ev.playbook} v${ev.version})`;
    case 'node_started':
      return `▶ node started${node}${ev.attempt ? ` attempt=${ev.attempt}` : ''}`;
    case 'attempt_started':
      return `  · attempt started${node} agent=${ev.agent ?? '?'} pid=${ev.pid ?? '?'}`;
    case 'attempt_finished': {
      const head = `  · attempt ${ev.status}${node} ${ev.duration_ms ?? '?'}ms`;
      return streamNodeOutput && ev.summary ? `${head} - ${truncate(ev.summary, 500)}` : head;
    }
    case 'node_finished': {
      const head = `✔ node ${ev.status}${node}`;
      if (streamNodeOutput && ev.output) return `${head}\n${truncate(ev.output)}`;
      return head;
    }
    case 'edge_traversed':
      return `  → ${ev.from} → ${ev.to}`;
    case 'connector_call':
      return `  ⚙ connector ${ev.connector ?? '?'}.${ev.function ?? '?'} → ${ev.outcome ?? '?'}`;
    case 'review_requested':
      return `⏸ human review requested${node}`;
    case 'review_decided':
      return `▶ review decided${node}: ${ev.decision ?? '?'}`;
    case 'question_asked':
      return streamNodeOutput
        ? `⏸ question asked${node}: ${truncate(ev.question, 500)}`
        : `⏸ question asked${node}`;
    case 'question_answered':
      return `▶ question answered${node}`;
    case 'run_paused':
      return `⏸ run paused`;
    case 'run_finished':
      return `apb run finished: ${ev.outcome}`;
    case 'run_error':
      return `✖ run error: ${truncate(ev.message ?? JSON.stringify(ev), 2000)}`;
    case 'run_provenance':
      // The real payload carries origin/digest/execution_root - `profiles` is
      // present but is a per-profile bundle list, not the headline.
      return `  provenance: origin=${ev.origin ?? '?'} root=${ev.execution_root ?? '?'} digest=${
        typeof ev.digest === 'string' ? ev.digest.slice(0, 23) : '?'
      }`;
    default:
      return `  · ${t}${node}`;
  }
}

/** apb event -> Paperclip runtime event level. */
function levelFor(ev) {
  if (ev.type === 'run_error') return 'error';
  if (ev.status === 'failed' || ev.status === 'timed_out' || ev.outcome === 'failed') return 'error';
  if (ev.type === 'run_paused' || ev.type === 'review_requested' || ev.type === 'question_asked') return 'warn';
  return 'info';
}

// ---------------------------------------------------------------------------
// result mapping
// ---------------------------------------------------------------------------

/**
 * Best-effort human summary of a finished apb run.
 *
 * `detail.outputs` is a serialized BTreeMap, so its keys arrive in ALPHABETICAL
 * order, not completion order - "the last entry" is an arbitrary node. The
 * completion order only exists in the event stream, so the last node_finished
 * carrying output is what we lift.
 */
export function summarizeRun(detail, { playbook, runId, streamNodeOutput = true } = {}) {
  if (typeof detail?.answer === 'string' && detail.answer.trim()) return detail.answer.trim();
  if (typeof detail?.failure_reason === 'string' && detail.failure_reason.trim()) {
    return `apb run ${runId} (${playbook}) failed: ${detail.failure_reason.trim()}`;
  }
  if (streamNodeOutput) {
    const events = Array.isArray(detail?.events) ? detail.events : [];
    let lastOutput = null;
    for (const ev of events) {
      if (!ev || typeof ev !== 'object') continue;
      if (ev.type !== 'node_finished') continue;
      if (typeof ev.output === 'string' && ev.output.trim()) lastOutput = ev.output.trim();
    }
    if (lastOutput) return truncate(lastOutput, 2000);
  }
  return `apb run ${runId} (${playbook}) ended with status ${detail?.run_status ?? 'unknown'}.`;
}

function baseResult(overrides = {}) {
  return {
    exitCode: 1,
    signal: null,
    timedOut: false,
    usageBasis: 'per_run',
    provider: 'apb',
    ...overrides,
  };
}

/** resultJson shared by every path that knows which apb run it was talking to. */
function runResultJson(session, extra = {}) {
  return {
    apbRunId: session.apbRunId ?? null,
    apbPlaybook: session.apbPlaybook ?? null,
    apbProject: session.apbProject ?? null,
    apbWorkspaceId: session.apbWorkspaceId ?? null,
    ...extra,
  };
}

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

export async function execute(ctx) {
  const cfg = normalizeConfig(ctx.config ?? ctx.agent?.adapterConfig ?? {});
  const context = ctx.context && typeof ctx.context === 'object' ? ctx.context : {};
  const sessionParams = ctx.runtime?.sessionParams ?? {};
  const taskKey = typeof ctx.runtime?.taskKey === 'string' ? ctx.runtime.taskKey : null;

  const log = (chunk, stream = 'stdout') => safe(ctx.onLog, stream, `${chunk}\n`);
  const emit = (event) => safe(ctx.onEvent, event);
  const warn = (msg) => {
    void log(`apb adapter: ${msg}`, 'stderr');
  };

  // --- project is mandatory ------------------------------------------------
  if (!cfg.project) {
    const message =
      'apb adapter: `project` is not configured. Set adapterConfig.project to an apb project name ' +
      '(see `apb projects list`). There is no default, so a missing project can never silently ' +
      'dispatch into another project.';
    await log(message, 'stderr');
    return baseResult({ exitCode: 78, errorMessage: message, errorCode: 'APB_NO_PROJECT', summary: message });
  }

  const client = new ApbClient({ baseUrl: cfg.apbBaseUrl, apiKey: cfg.apbApiKey });

  // --- re-attach to a still-live run from a previous wake ------------------
  // Without this, a wake that timed out / returned on pause / gave up leaves a
  // live apb run behind and the NEXT wake starts a second one. That is the
  // run-amplification loop; re-attaching collapses it.
  const prior = {
    apbRunId: asString(sessionParams?.apbRunId, '') || null,
    apbWorkspaceId: asString(sessionParams?.apbWorkspaceId, '') || null,
    apbPlaybook: asString(sessionParams?.apbPlaybook, '') || null,
    apbProject: asString(sessionParams?.apbProject, '') || null,
  };
  if (prior.apbRunId && prior.apbWorkspaceId) {
    try {
      const existing = await client.getRun(prior.apbRunId, prior.apbWorkspaceId);
      if (existing && isLiveRunStatus(existing.run_status)) {
        await log(
          `apb adapter: re-attaching to live apb run ${prior.apbRunId} ` +
            `(status ${existing.run_status}) instead of starting a new one`,
        );
        await emit({
          eventType: 'apb_run_reattached',
          level: 'info',
          message: `re-attached to apb run ${prior.apbRunId}`,
          payload: { ...prior, runStatus: existing.run_status },
        });
        return await pollToCompletion({
          client,
          cfg,
          ctx,
          log,
          emit,
          session: prior,
          playbook: prior.apbPlaybook ?? 'unknown',
          seedDetail: existing,
        });
      }
      await log(
        `apb adapter: previous apb run ${prior.apbRunId} is ${existing?.run_status ?? 'gone'}; starting a new run`,
      );
    } catch (err) {
      // A vanished run (404) or an unreachable engine must not block a fresh
      // start - fall through and let the normal path report any real problem.
      await log(
        `apb adapter: could not re-attach to ${prior.apbRunId} (${err.code ?? 'error'}); starting a new run`,
      );
    }
  }

  // --- resolve what to run -------------------------------------------------
  let playbook;
  let via;
  try {
    ({ playbook, via } = resolvePlaybook({ adapterConfig: cfg, context, sessionParams, taskKey, warn }));
  } catch (err) {
    const message = err instanceof ResolutionError ? err.message : String(err?.message ?? err);
    await log(`apb adapter: ${message}`, 'stderr');
    return baseResult({
      exitCode: 78,
      errorMessage: message,
      errorCode: err?.code ?? 'APB_NO_PLAYBOOK',
      summary: message,
    });
  }

  const params = resolveParams({ adapterConfig: cfg, context, ctx, taskKey, warn });
  const instruction = resolveInstruction({ adapterConfig: cfg, context, ctx, taskKey });

  // --- resolve the apb workspace and validate the playbook exists ----------
  let workspace;
  try {
    workspace = await client.resolveWorkspace(cfg.project);
    // NOT cached, deliberately. `GET /api/playbooks` returns every reachable
    // workspace's playbooks because apb ignores a `project` query param, so
    // this is one wide read per run. It stays uncached because a playbook can
    // be added, renamed or frozen between wakes, and a stale cache would report
    // "playbook not found" for one that exists (or start one that no longer
    // does). Two extra loopback GETs per run is the cheaper error.
    const playbooks = await client.listPlaybooks();
    const known = playbooks.filter((p) => p.workspace_id === workspace.workspace_id);
    if (!known.some((p) => p.id === playbook)) {
      const message =
        `apb playbook "${playbook}" (resolved via ${via}) does not exist in project "${cfg.project}". ` +
        `Known: ${known.map((p) => p.id).join(', ') || 'none'}`;
      await log(`apb adapter: ${message}`, 'stderr');
      return baseResult({ exitCode: 78, errorMessage: message, errorCode: 'APB_PLAYBOOK_NOT_FOUND', summary: message });
    }
  } catch (err) {
    return await failFromApbError(err, { log, playbook, project: cfg.project, baseUrl: cfg.apbBaseUrl });
  }

  await log(
    `apb adapter: dispatching playbook "${playbook}" (via ${via}) to project "${cfg.project}" ` +
      `[workspace ${workspace.workspace_id}] at ${safeUrl(cfg.apbBaseUrl)}`,
  );
  await log(`apb adapter: params ${describeParams(params, cfg)}`);

  await safe(ctx.onMeta, {
    adapterType: type,
    command: `POST ${safeUrl(cfg.apbBaseUrl)}/api/playbooks/${playbook}/run?workspace=${workspace.workspace_id}`,
    cwd: workspace.path,
    context: { playbook, project: cfg.project, workspaceId: workspace.workspace_id, resolvedVia: via },
  });

  // --- start the run -------------------------------------------------------
  let apbRunId;
  try {
    apbRunId = await client.startRun(playbook, workspace.workspace_id, { instruction, params });
  } catch (err) {
    return await failFromApbError(err, { log, playbook, project: cfg.project, baseUrl: cfg.apbBaseUrl });
  }

  const session = {
    apbRunId,
    apbPlaybook: playbook,
    apbProject: cfg.project,
    apbWorkspaceId: workspace.workspace_id,
  };

  await log(`apb adapter: started apb run ${apbRunId}`);
  await emit({ eventType: 'apb_run_started', level: 'info', message: `apb run ${apbRunId} started`, payload: session });

  return await pollToCompletion({ client, cfg, ctx, log, emit, session, playbook });
}

/**
 * Poll an apb run to a terminal state, streaming its journal.
 * Shared by the fresh-start and re-attach paths.
 */
async function pollToCompletion({ client, cfg, ctx, log, emit, session, playbook, seedDetail = null }) {
  const { apbRunId, apbWorkspaceId } = session;
  const deadline = Date.now() + cfg.timeoutMs;
  const seen = new Set();
  let anonymousEventCount = 0;
  let detail = seedDetail;
  let lastPollError = null;
  let failingSince = null;

  const drain = async () => {
    const events = Array.isArray(detail?.events) ? detail.events : [];
    for (const ev of events) {
      if (!ev || typeof ev !== 'object') continue; // null/garbage element must not throw
      // seq is normally a monotonic integer, but an event without one would
      // collapse every such event onto the key `undefined` and drop all but the
      // first. Give those a positional key instead.
      const key = Number.isFinite(ev.seq) ? `s${ev.seq}` : `a${anonymousEventCount++}`;
      if (seen.has(key)) continue;
      seen.add(key);
      await log(renderEvent(ev, cfg));
      await emit({
        eventType: `apb.${ev.type ?? 'unknown'}`.slice(0, 120),
        level: levelFor(ev),
        message: renderEvent(ev, { streamNodeOutput: false }),
        payload: scrubEvent(ev, cfg),
      });
    }
  };

  if (detail) await drain();

  while (Date.now() < deadline) {
    if (detail && APB_STOP_POLLING_STATES.has(detail.run_status)) break;

    if (detail) {
      const status = detail.run_status;
      if (status === 'paused' && cfg.onPause === 'return') {
        const waiting = detail.progress?.waiting_kind ?? 'a gate';
        const node = detail.progress?.waiting_on ?? '?';
        const message =
          `apb run ${apbRunId} is paused on ${waiting} at node "${node}". ` +
          `Decide it with \`apb review\` / \`apb answer\`, or POST /api/runs/${apbRunId}/review. ` +
          `The next wake will re-attach to this run rather than starting a new one.`;
        await log(`apb adapter: ${message}`);
        return baseResult({
          exitCode: 75, // EX_TEMPFAIL - resumable
          // Paperclip renders a generic "Adapter failed" for any non-zero exit
          // with no errorMessage, so say what actually happened.
          errorMessage: message,
          errorCode: 'APB_RUN_PAUSED',
          sessionParams: session,
          sessionDisplayId: apbRunId,
          summary: message,
          resultJson: runResultJson(session, { apbRunStatus: status, progress: detail.progress ?? null, stillLive: true }),
        });
      }
      await sleep(cfg.pollIntervalMs);
    }

    try {
      const next = await client.getRun(apbRunId, apbWorkspaceId);
      // A 200 with an empty body yields null; treating that as a run detail
      // would throw outside any try and reject execute(), orphaning the run.
      if (!next || typeof next !== 'object') {
        throw new ApbError(`apb returned an empty run detail for ${apbRunId}`, { code: 'APB_BAD_RESPONSE' });
      }
      detail = next;
      failingSince = null;
      lastPollError = null;
    } catch (err) {
      // Duration-based, not count-based: at the 250ms floor a count of 10 gave
      // up after ~2.5s, so an ordinary apb restart abandoned a live run.
      lastPollError = err;
      const firstFailure = failingSince === null;
      failingSince ??= Date.now();
      const failingFor = Date.now() - failingSince;
      if (firstFailure) {
        await log(`apb adapter: poll failed (${err.code ?? 'error'}): ${err.message}`, 'stderr');
      }
      if (failingFor >= cfg.pollGiveUpMs) {
        const message =
          `apb stayed unreachable for ${Math.round(failingFor / 1000)}s while run ${apbRunId} was in flight ` +
          `(${err.message}). The apb run MAY STILL BE LIVE - check \`apb runs\` and stop it with ` +
          `\`apb stop ${apbRunId}\` if unwanted. The next wake will re-attach if it is still running.`;
        await log(`apb adapter: ${message}`, 'stderr');
        return baseResult({
          exitCode: 1,
          errorMessage: message,
          errorCode: err.code ?? 'APB_UNREACHABLE',
          errorFamily: 'transient_upstream',
          sessionParams: session,
          sessionDisplayId: apbRunId,
          summary: message,
          resultJson: runResultJson(session, {
            apbRunStatus: detail?.run_status ?? null,
            stillLive: true,
            gaveUpAfterMs: failingFor,
          }),
        });
      }
      await sleep(cfg.pollIntervalMs);
      continue;
    }

    await drain();
  }

  // --- timeout -------------------------------------------------------------
  if (!detail || !APB_STOP_POLLING_STATES.has(detail.run_status)) {
    const message =
      `Timed out after ${cfg.timeoutMs}ms waiting for apb run ${apbRunId} (${playbook}). ` +
      `apb exposes no HTTP stop endpoint, so the run is STILL RUNNING - stop it with \`apb stop ${apbRunId}\` ` +
      `if unwanted. The next wake will re-attach to it rather than starting a second run.`;
    await log(`apb adapter: ${message}`, 'stderr');
    return baseResult({
      exitCode: null,
      timedOut: true,
      errorMessage: lastPollError ? `${message} Last poll error: ${lastPollError.message}` : message,
      errorCode: 'APB_RUN_TIMEOUT',
      sessionParams: session,
      sessionDisplayId: apbRunId,
      summary: message,
      resultJson: runResultJson(session, { apbRunStatus: detail?.run_status ?? null, stillLive: true }),
    });
  }

  // --- terminal ------------------------------------------------------------
  const status = detail.run_status;
  const summary = summarizeRun(detail, { playbook, runId: apbRunId, streamNodeOutput: cfg.streamNodeOutput });
  const exitCode = exitCodeForRunStatus(status);
  await log(`apb adapter: apb run ${apbRunId} ended "${status}" -> exit ${exitCode}`);

  return baseResult({
    exitCode,
    model: playbook,
    sessionParams: session,
    sessionDisplayId: apbRunId,
    summary,
    errorMessage: status === 'succeeded' ? null : summary,
    errorCode: status === 'succeeded' ? null : `APB_RUN_${String(status).toUpperCase()}`,
    resultJson: runResultJson(session, {
      apbRunStatus: status,
      apbPlaybookVersion: detail.version ?? null,
      failureReason: detail.failure_reason ?? null,
      // `nodes` is a node->status map, never output text, so it is safe to
      // include regardless of streamNodeOutput.
      nodes: detail.nodes ?? null,
      answer: detail.answer ?? null,
    }),
  });
}

/** Turn an ApbError raised before/at start into a Paperclip result. */
async function failFromApbError(err, { log, playbook, project, baseUrl }) {
  const code = err instanceof ApbError ? err.code : 'APB_ERROR';
  let message;
  let exitCode = 1;

  switch (code) {
    case 'APB_UNREACHABLE':
      message = `apb engine is not reachable at ${safeUrl(baseUrl)}. Is apb-server running? (${err.message})`;
      exitCode = 69; // EX_UNAVAILABLE
      break;
    case 'APB_PROJECT_NOT_FOUND':
      message = err.message;
      exitCode = 78;
      break;
    case 'APB_PERMIT_REFUSED':
      message =
        `apb refused to start "${playbook}" in "${project}": the connector-trust gate rejected it. ` +
        `A playbook binding connectors through a profile with trusted:false needs an explicit approval first ` +
        `(\`apb connector\` approve, or POST /api/connectors/approve). apb's HTTP run endpoint has no ` +
        `acknowledge parameter - that exists only on the MCP tool. Detail: ${JSON.stringify(err.body)}`;
      exitCode = 77; // EX_NOPERM
      break;
    case 'APB_WORKDIR_BUSY':
      message = `apb workdir for "${project}" is busy and queueing is disabled: ${err.message}`;
      exitCode = 75;
      break;
    case 'APB_NOT_FOUND':
      message = `apb could not find playbook "${playbook}" in "${project}": ${err.message}`;
      exitCode = 78;
      break;
    default:
      message = `apb request failed: ${err.message}`;
  }

  await log(`apb adapter: ${message}`, 'stderr');
  return baseResult({
    exitCode,
    errorMessage: message,
    errorCode: code,
    errorFamily: code === 'APB_UNREACHABLE' ? 'transient_upstream' : undefined,
    summary: message,
  });
}

// ---------------------------------------------------------------------------
// testEnvironment
// ---------------------------------------------------------------------------

export async function testEnvironment(ctx) {
  const checks = [];
  const cfg = normalizeConfig(ctx?.config ?? {});
  const adapterType = ctx?.adapterType ?? type;
  const push = (code, level, message, extra = {}) => checks.push({ code, level, message, ...extra });
  const done = () => ({
    adapterType,
    status: checks.some((c) => c.level === 'error') ? 'fail' : checks.some((c) => c.level === 'warn') ? 'warn' : 'pass',
    checks,
    testedAt: new Date().toISOString(),
  });

  let url = null;
  try {
    url = new URL(cfg.apbBaseUrl);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') throw new Error('bad protocol');
  } catch {
    push('apb_base_url_invalid', 'error', `apbBaseUrl must be an http:// or https:// URL (got "${safeUrl(cfg.apbBaseUrl)}").`);
  }

  if (!cfg.project) {
    push('apb_project_missing', 'error', 'adapterConfig.project is required and has no default.', {
      hint: 'Set it to an apb project name from `apb projects list`.',
    });
  }

  if (url) {
    const loopback = isLoopbackHost(url.hostname);
    if (url.username || url.password) {
      push('apb_base_url_userinfo', 'warn', 'apbBaseUrl embeds credentials in its userinfo component.', {
        hint: 'Use the apbApiKey field instead; userinfo leaks through logs and proxies.',
      });
    }
    if (url.protocol === 'http:' && !loopback) {
      push('apb_plain_http_remote', 'warn', `apb is reached over plain HTTP at a non-loopback host (${url.hostname}).`, {
        hint: 'apb never terminates TLS itself. Front a remote apb with HTTPS and issue an API key (`apb server key issue`).',
      });
    }
    if (!cfg.apbApiKey && !loopback) {
      push('apb_api_key_missing', 'warn', 'No apbApiKey set for a non-loopback apb.', {
        hint: 'apb enables auth as soon as one key exists; a remote bind requires at least one.',
      });
    }
  }

  if (cfg.allowTextDirectives) {
    push('apb_text_directives_enabled', 'warn', 'allowTextDirectives is ON: issue text can choose the playbook and set params.', {
      hint: 'Issue titles, descriptions and comments are attacker-controllable. Enable only where every issue author is trusted.',
    });
  }

  if (!url || !cfg.project) return done();

  const client = new ApbClient({ baseUrl: cfg.apbBaseUrl, apiKey: cfg.apbApiKey, requestTimeoutMs: 5000 });

  // 1. reachability
  try {
    const health = await client.health();
    push('apb_health_ok', 'info', `apb health endpoint reachable at ${safeUrl(cfg.apbBaseUrl)} (status=${health?.status}).`);
  } catch (err) {
    push('apb_health_unreachable', 'error', `Could not reach the apb health endpoint at ${safeUrl(cfg.apbBaseUrl)}.`, {
      detail: err.message,
      hint: 'Start the engine (systemctl status apb-server) or fix apbBaseUrl.',
    });
    return done();
  }

  // 2. project resolves to a workspace
  let workspace = null;
  try {
    workspace = await client.resolveWorkspace(cfg.project);
    push('apb_project_ok', 'info', `apb project "${cfg.project}" resolves to ${workspace.workspace_id} (${workspace.path}).`);
  } catch (err) {
    push('apb_project_not_found', 'error', err.message, {
      hint: 'Register the project by running any apb command inside its directory, then re-test.',
    });
    return done();
  }

  // 3. configured playbooks exist
  try {
    const all = await client.listPlaybooks();
    const known = all.filter((p) => p.workspace_id === workspace.workspace_id);
    const ids = new Set(known.map((p) => p.id));
    push('apb_playbooks_listed', 'info', `Project "${cfg.project}" exposes ${ids.size} playbook(s): ${[...ids].join(', ') || 'none'}.`);

    const wanted = new Set();
    if (cfg.playbook) wanted.add(cfg.playbook);
    for (const [k, v] of Object.entries(cfg.playbookMap ?? {})) {
      if (typeof v === 'string' && v.trim()) wanted.add(v.trim());
      else push('apb_playbook_map_invalid', 'warn', `playbookMap["${k}"] is not a string and will be ignored.`);
    }

    if (!wanted.size) {
      push('apb_no_playbook_configured', 'warn', 'Neither playbook nor playbookMap is set.', {
        hint: cfg.allowTextDirectives
          ? 'A wake can only run if the task text carries an `apb:playbook=<id>` directive.'
          : 'With allowTextDirectives off, every wake will fail to resolve a playbook.',
      });
    }
    for (const w of wanted) {
      if (ids.has(w)) push('apb_playbook_ok', 'info', `Playbook "${w}" exists in "${cfg.project}".`);
      else
        push('apb_playbook_not_found', 'error', `Configured playbook "${w}" does not exist in "${cfg.project}".`, {
          hint: `Known playbooks: ${[...ids].join(', ') || 'none'}.`,
        });
    }
  } catch (err) {
    push('apb_playbooks_unavailable', 'error', `Could not list apb playbooks: ${err.message}`);
  }

  // 4. advisory: profiles with trusted:false make connector-binding playbooks
  //    fail the start with a 409 that this adapter cannot resolve on its own.
  try {
    const profiles = await client.listProfiles(workspace.workspace_id);
    const untrusted = profiles.filter((p) => p.trusted === false).map((p) => p.name);
    if (untrusted.length) {
      push(
        'apb_untrusted_profiles',
        'warn',
        `Project "${cfg.project}" has ${untrusted.length} profile(s) with trusted:false: ${untrusted.join(', ')}.`,
        {
          hint:
            'A playbook that binds connectors through an untrusted profile is refused at start with HTTP 409. ' +
            "apb's HTTP run endpoint has no acknowledge parameter (that exists only on the MCP tool), so approve " +
            'the connector out of band (`apb connector` / POST /api/connectors/approve) or mark the profile trusted.',
        },
      );
    } else if (profiles.length) {
      push('apb_profiles_trusted', 'info', `All ${profiles.length} profile(s) in "${cfg.project}" are trusted.`);
    }
  } catch (err) {
    push('apb_profiles_unavailable', 'warn', `Could not list apb profiles for the trust advisory: ${err.message}`);
  }

  return done();
}

// ---------------------------------------------------------------------------
// session codec
// ---------------------------------------------------------------------------

const readString = (v) => (typeof v === 'string' && v.trim() ? v.trim() : null);

/**
 * Keys the SERVER owns inside sessionParams (heartbeat.js ~L3005-3009:
 * __paperclipConfiguredModel, __paperclipConfigFingerprint, ...). The server
 * calls `sessionCodec.deserialize()` BEFORE stripping this metadata and
 * re-attaches it after `serialize()`, so a strict allowlist would destroy it
 * and break model-change reset and config-freshness detection. Pass it through.
 */
const PAPERCLIP_META_PREFIX = '__paperclip';

function pickSession(r) {
  const out = {};
  for (const [k, v] of Object.entries(r)) {
    if (k.startsWith(PAPERCLIP_META_PREFIX)) out[k] = v;
  }
  const apbRunId = readString(r.apbRunId);
  const apbPlaybook = readString(r.apbPlaybook);
  const apbProject = readString(r.apbProject);
  const apbWorkspaceId = readString(r.apbWorkspaceId);
  if (apbRunId) out.apbRunId = apbRunId;
  if (apbPlaybook) out.apbPlaybook = apbPlaybook;
  if (apbProject) out.apbProject = apbProject;
  if (apbWorkspaceId) out.apbWorkspaceId = apbWorkspaceId;
  // Nothing of ours and nothing of the server's -> no session at all.
  return Object.keys(out).length ? out : null;
}

export const sessionCodec = {
  deserialize(raw) {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
    return pickSession(raw);
  },
  serialize(params) {
    if (!params || typeof params !== 'object' || Array.isArray(params)) return null;
    return pickSession(params);
  },
  getDisplayId(params) {
    if (!params) return null;
    return readString(params.apbRunId);
  },
};

// ---------------------------------------------------------------------------
// module factory
// ---------------------------------------------------------------------------

export const agentConfigurationDoc = `# apb agent configuration

Adapter: apb

Dispatches a Paperclip wake to a playbook run on a local agentic-playbooks (apb)
engine, streams the apb journal into the Paperclip run log, and maps the final
apb run state onto the Paperclip result.

Required fields:
- project (string): apb project name, as listed by \`apb projects list\`.
  REQUIRED - there is no default.

At least one of:
- playbook (string): default playbook id.
- playbookMap (JSON): taskKey / issue identifier / issue id / wakeReason ->
  playbook id. Supports "PREFIX-*" globs and a "default" key.

Optional fields:
- apbBaseUrl (string): defaults to http://127.0.0.1:7321.
- apbApiKey (string): only when apb server mode has issued keys.
- params (JSON), instruction (string): defaults for the run.
- timeoutMs, pollIntervalMs, pollGiveUpMs (numbers).
- onPause (return|wait), streamNodeOutput (bool), logParamValues (bool).
- allowTextDirectives (bool, default false): SECURITY-SENSITIVE, see below.

Runtime mapping:
- Starts runs with POST /api/playbooks/{id}/run?workspace={workspace_id}.
- Polls GET /api/runs/{run_id}?workspace={workspace_id} and streams newly-seen
  journal events. apb has no incremental log endpoint and its WebSocket carries
  only contentless change pings, so polling is the reliable transport.
- If a previous wake left a live apb run in sessionParams, the next wake
  RE-ATTACHES to it instead of starting a second run.
- apb has NO HTTP stop endpoint: on timeout the apb run keeps running and must
  be stopped with \`apb stop <run-id>\`.

Security guidance:
- Keep apb on loopback, or front it with HTTPS and an issued API key.
- allowTextDirectives lets issue text pick the playbook and its params. Issue
  text is attacker-controllable; leave it off unless all authors are trusted.
- streamNodeOutput=false withholds apb node output from logs, event payloads
  and the summary.
`;

export function createServerAdapter() {
  return {
    type,
    label,
    execute,
    testEnvironment,
    sessionCodec,
    getConfigSchema,
    agentConfigurationDoc,
    models: [],
    // apb runs are dispatched over apb's own API; the adapter never needs a
    // Paperclip run JWT, so it does not opt into local agent JWT injection.
    supportsLocalAgentJwt: false,
    supportsInstructionsBundle: false,
    requiresMaterializedRuntimeSkills: false,
  };
}

export default { createServerAdapter };
