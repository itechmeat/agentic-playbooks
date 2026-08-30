/**
 * Minimal HTTP client for the apb (agentic-playbooks) engine API.
 *
 * Verified against apb 0.20.2, server `apb dashboard --port 7321`.
 * Route table lives in crates/apb-server/src/lib.rs::build_router.
 *
 * Endpoints used by the adapter:
 *   GET  /api/health                              -> {"status":"ok"}
 *   GET  /api/projects                            -> [{name,path,playbook_count,workspace_id}]
 *   GET  /api/playbooks                           -> [{id,name,description,current,versions,frozen,project,workspace_id}]
 *   POST /api/playbooks/{id}/run?workspace=<ws>   -> {"run_id": "..."}
 *   GET  /api/runs/{id}?workspace=<ws>            -> full run detail (events, nodes, outputs, run_status, answer)
 *
 * IMPORTANT QUIRK: the apb server serves a SPA and uses `.fallback(static_handler)`.
 * An unknown /api/* path therefore returns HTTP 200 with `text/html` (index.html),
 * NOT a 404. Every read here validates that the body actually parses as JSON so a
 * typo'd path surfaces as a clear error instead of silently "succeeding".
 */

/**
 * Defence in depth: never let a URL's userinfo reach an error string. Errors
 * from below us (fetch, DNS, TLS) routinely quote the offending URL verbatim,
 * and those strings end up in Paperclip run logs.
 */
export function scrubCredentials(text) {
  return String(text ?? '').replace(/\b([a-z][a-z0-9+.-]*:\/\/)([^/\s:@]+)(?::([^/\s@]*))?@/gi, '$1***:***@');
}

export class ApbError extends Error {
  constructor(message, { status = null, code = null, body = null } = {}) {
    super(scrubCredentials(message));
    this.name = 'ApbError';
    this.status = status;
    this.code = code;
    this.body = body;
  }
}

const SLASH = '/'.charCodeAt(0);

/**
 * Trim trailing slashes in linear time.
 *
 * The obvious `replace(/\/+$/, '')` is quadratic when a long run of slashes is
 * followed by anything other than end-of-string: the engine starts a match at
 * every offset in the run, greedily consumes the rest of it, then fails the `$`
 * anchor and does it again one offset along. baseUrl is operator-supplied
 * config that this package does not length-check, so a value like 80KB of
 * slashes and a trailing character blocks the event loop for about six seconds.
 * Scanning back to the last non-slash and slicing once is linear and allocates
 * a single string.
 */
export function stripTrailingSlashes(value) {
  const text = String(value ?? '');
  let end = text.length;
  while (end > 0 && text.charCodeAt(end - 1) === SLASH) end -= 1;
  return end === text.length ? text : text.slice(0, end);
}

const DEFAULT_BASE_URL = 'http://127.0.0.1:7321';

export class ApbClient {
  /**
   * @param {object} opts
   * @param {string} [opts.baseUrl]       apb API base, default http://127.0.0.1:7321
   * @param {number} [opts.requestTimeoutMs] per-request timeout, default 15000
   * @param {string} [opts.apiKey]        optional bearer key when apb runs in server mode
   * @param {typeof fetch} [opts.fetchImpl]
   */
  constructor({ baseUrl = DEFAULT_BASE_URL, requestTimeoutMs = 15000, apiKey = null, fetchImpl = fetch } = {}) {
    // Credentials in the URL are stripped up front and carried as a Basic
    // Authorization header instead. Two reasons: fetch() REFUSES to construct a
    // Request from a credentialed URL (so it would fail outright), and its
    // error message quotes the whole URL - password included - into anything
    // that logs the failure.
    let base = stripTrailingSlashes(baseUrl);
    let basic = null;
    try {
      const u = new URL(base);
      if (u.username || u.password) {
        basic = Buffer.from(`${decodeURIComponent(u.username)}:${decodeURIComponent(u.password)}`).toString('base64');
        u.username = '';
        u.password = '';
        base = stripTrailingSlashes(u.toString());
      }
    } catch {
      /* leave a malformed base alone; the first request reports it */
    }
    this.baseUrl = base;
    this.requestTimeoutMs = requestTimeoutMs;
    this.apiKey = apiKey;
    this.basicAuth = basic;
    this.fetchImpl = fetchImpl;
  }

  #headers(extra = {}) {
    // apb's CSRF second layer only applies to cookie-authenticated requests,
    // but the header is inert otherwise and keeps us compatible if someone
    // fronts apb with a session cookie instead of a bearer key.
    const h = { accept: 'application/json', 'x-requested-with': 'apb-dashboard', ...extra };
    // apb turns auth ON iff at least one API key exists in server-auth.yaml
    // (not based on bind address). On the default loopback bind with no keys
    // issued, every request passes through unauthenticated.
    if (this.apiKey) h.authorization = `Bearer ${this.apiKey}`;
    else if (this.basicAuth) h.authorization = `Basic ${this.basicAuth}`;
    return h;
  }

  async #request(method, path, { body = null, timeoutMs = null, signal = null } = {}) {
    const url = `${this.baseUrl}${path}`;
    const ac = new AbortController();
    const budget = timeoutMs ?? this.requestTimeoutMs;
    const timer = setTimeout(() => ac.abort(new Error(`apb request timeout after ${budget}ms`)), budget);
    const onOuterAbort = () => ac.abort(signal.reason);
    if (signal) {
      if (signal.aborted) ac.abort(signal.reason);
      else signal.addEventListener('abort', onOuterAbort, { once: true });
    }

    let res;
    let text;
    try {
      res = await this.fetchImpl(url, {
        method,
        headers: this.#headers(body ? { 'content-type': 'application/json' } : {}),
        body: body ? JSON.stringify(body) : undefined,
        signal: ac.signal,
      });
      // The body read stays INSIDE this try and BEFORE the timer is cleared:
      // headers can arrive promptly and the body then stall or reset, and an
      // unguarded read would both hang past the deadline and surface as a raw
      // TypeError that the caller cannot classify.
      text = await res.text();
    } catch (err) {
      // Abort wins over the network classification: an aborted in-flight
      // request also surfaces as a TypeError from fetch. The reason object we
      // pass to abort() is not necessarily an AbortError, so trust the signal.
      if (ac.signal.aborted) {
        const outerAborted = signal?.aborted === true;
        throw new ApbError(
          `apb request aborted: ${method} ${path} (${ac.signal.reason?.message || 'aborted'})`,
          { code: outerAborted ? 'APB_ABORTED' : 'APB_REQUEST_TIMEOUT' },
        );
      }
      const netCode = networkErrorCode(err);
      if (netCode) {
        throw new ApbError(`apb engine unreachable at ${this.baseUrl} (${netCode})`, { code: 'APB_UNREACHABLE' });
      }
      throw new ApbError(`apb request failed: ${method} ${path}: ${err.message}`, { code: 'APB_REQUEST_FAILED' });
    } finally {
      clearTimeout(timer);
      if (signal) signal.removeEventListener('abort', onOuterAbort);
    }

    const contentType = res.headers.get('content-type') || '';

    if (!res.ok) {
      // apb returns plain-text bodies for most error arms and a JSON refusal
      // object for the connector-permit 409.
      let parsed = null;
      if (contentType.includes('application/json')) {
        try {
          parsed = JSON.parse(text);
        } catch {
          /* fall through to text */
        }
      }
      throw new ApbError(`apb ${method} ${path} -> HTTP ${res.status}: ${text.slice(0, 500)}`, {
        status: res.status,
        code: apbErrorCode(res.status),
        body: parsed ?? text,
      });
    }

    // Guard against the SPA fallback answering 200 for an unknown route.
    // apb 0.20.2 serves index.html from the static fallback as
    // `application/octet-stream` (verified live), NOT text/html, so the
    // content-type test alone would never fire - the doctype sniff is what
    // actually catches it. Both are kept: a future apb may label it correctly.
    // The doctype sniff is the reliable signal; the content-type test only
    // helps if apb ever starts labelling the fallback honestly. Deliberately
    // NOT keying on octet-stream alone: that would reject a valid JSON body
    // that merely carries a sloppy content-type.
    const looksLikeSpa = /^\s*<!doctype html/i.test(text) || contentType.includes('text/html');
    if (looksLikeSpa) {
      throw new ApbError(
        `apb ${method} ${path} returned the dashboard SPA instead of JSON - the route does not exist on this apb build`,
        { status: res.status, code: 'APB_ROUTE_NOT_FOUND' },
      );
    }
    if (!text.trim()) return null;
    try {
      return JSON.parse(text);
    } catch {
      throw new ApbError(`apb ${method} ${path} returned non-JSON body: ${text.slice(0, 200)}`, {
        status: res.status,
        code: 'APB_BAD_RESPONSE',
      });
    }
  }

  /** GET /api/health */
  async health(opts = {}) {
    return this.#request('GET', '/api/health', opts);
  }

  /** GET /api/projects */
  async listProjects(opts = {}) {
    const out = await this.#request('GET', '/api/projects', opts);
    return Array.isArray(out) ? out : [];
  }

  /**
   * GET /api/playbooks
   * NOTE: the server ignores a `project` query param and always returns every
   * reachable workspace's playbooks, so filtering is done here on the
   * `workspace_id` / `project` fields carried by each entry.
   */
  async listPlaybooks(opts = {}) {
    const out = await this.#request('GET', '/api/playbooks', opts);
    return Array.isArray(out) ? out : [];
  }

  /**
   * GET /api/profiles?workspace=<ws> -> {profiles:[{name,scope,trusted,agent,model,...}]}
   * Used only for an advisory trust check in testEnvironment.
   */
  async listProfiles(workspaceId, opts = {}) {
    const out = await this.#request(
      'GET',
      `/api/profiles?workspace=${encodeURIComponent(workspaceId)}`,
      opts,
    );
    return Array.isArray(out?.profiles) ? out.profiles : [];
  }

  /** Resolve a project NAME (as shown in `apb projects list`) to its workspace_id. */
  async resolveWorkspace(projectName, opts = {}) {
    const projects = await this.listProjects(opts);
    const hit = projects.find((p) => p.name === projectName);
    if (!hit) {
      throw new ApbError(
        `apb project "${projectName}" is not in the workspace registry (known: ${
          projects.map((p) => p.name).join(', ') || 'none'
        })`,
        { code: 'APB_PROJECT_NOT_FOUND' },
      );
    }
    return hit;
  }

  /**
   * POST /api/playbooks/{id}/run?workspace=<ws>
   * @param {string} playbookId
   * @param {string} workspaceId
   * @param {object} [body] {instruction?: string, params?: Record<string,string>, continued_from?: string}
   * @returns {Promise<string>} run_id
   */
  async startRun(playbookId, workspaceId, body = {}, opts = {}) {
    const params = stringifyParams(body.params);
    const payload = {};
    if (body.instruction) payload.instruction = body.instruction;
    if (Object.keys(params).length) payload.params = params;
    if (body.continued_from) payload.continued_from = body.continued_from;

    const out = await this.#request(
      'POST',
      `/api/playbooks/${encodeURIComponent(playbookId)}/run?workspace=${encodeURIComponent(workspaceId)}`,
      { body: payload, ...opts },
    );
    if (!out || typeof out.run_id !== 'string') {
      throw new ApbError(`apb run start returned no run_id: ${JSON.stringify(out)}`, { code: 'APB_BAD_RESPONSE' });
    }
    return out.run_id;
  }

  /** GET /api/runs/{id}?workspace=<ws> - full run detail. */
  async getRun(runId, workspaceId, opts = {}) {
    return this.#request(
      'GET',
      `/api/runs/${encodeURIComponent(runId)}?workspace=${encodeURIComponent(workspaceId)}`,
      opts,
    );
  }
}

/** apb params are typed `BTreeMap<String,String>` server-side: coerce everything to a string. */
export function stringifyParams(params) {
  const out = {};
  if (!params || typeof params !== 'object') return out;
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null) continue;
    out[String(k)] = typeof v === 'string' ? v : JSON.stringify(v);
  }
  return out;
}

const NET_CODES = /^(ECONNREFUSED|ENOTFOUND|EHOSTUNREACH|ENETUNREACH|ECONNRESET|EPIPE|EAI_AGAIN|UND_ERR_SOCKET|UND_ERR_CONNECT_TIMEOUT)$/;

/**
 * Node's fetch wraps connection failures in a TypeError whose `cause` may
 * itself be an AggregateError holding the real errno (one per resolved
 * address). Walk the chain rather than trusting `err.cause.code`.
 */
function networkErrorCode(err, depth = 0) {
  if (!err || depth > 5) return null;
  if (typeof err.code === 'string' && NET_CODES.test(err.code)) return err.code;
  if (Array.isArray(err.errors)) {
    for (const sub of err.errors) {
      const hit = networkErrorCode(sub, depth + 1);
      if (hit) return hit;
    }
  }
  return networkErrorCode(err.cause, depth + 1);
}

function apbErrorCode(status) {
  switch (status) {
    case 404:
      return 'APB_NOT_FOUND';
    case 409:
      // Emitted by the connector-permit gate: a playbook binding connectors
      // through an untrusted profile needs an explicit acknowledge first.
      return 'APB_PERMIT_REFUSED';
    case 422:
      return 'APB_INVALID_REQUEST';
    case 429:
      return 'APB_WORKDIR_BUSY';
    default:
      return status >= 500 ? 'APB_SERVER_ERROR' : 'APB_HTTP_ERROR';
  }
}

/**
 * apb run states (apb-engine/src/state.rs::RunStatus), single source of truth.
 *
 * The engine's own terminal set is {succeeded, failed, aborted}. `interrupted`
 * is not terminal to the engine - it means the driver process went away and the
 * run could in principle be resumed with `apb resume`. The status the API
 * reports is liveness-corrected, so a healthy in-flight run always reads
 * `running`: seeing `interrupted` means the driver is provably gone, and there
 * is nothing left for this adapter to wait for. So the adapter stops polling on
 * `interrupted` too, and reports it as a distinct failure.
 *
 * `APB_STOP_POLLING_STATES` is the only set the runtime consults; the others
 * describe the engine's own vocabulary and are used for validation and docs.
 */
export const APB_ALL_STATES = Object.freeze([
  'created',
  'running',
  'paused',
  'succeeded',
  'failed',
  'aborted',
  'interrupted',
]);
/** Terminal to the apb engine itself. */
export const APB_TERMINAL_STATES = new Set(['succeeded', 'failed', 'aborted']);
/** Terminal *to this adapter* - nothing further will happen without an operator. */
export const APB_STOP_POLLING_STATES = new Set([...APB_TERMINAL_STATES, 'interrupted']);
/** A run in one of these may still make progress on its own. */
export const APB_LIVE_STATES = new Set(APB_ALL_STATES.filter((s) => !APB_STOP_POLLING_STATES.has(s)));

/** True when `status` describes a run that is still (or could still be) alive. */
export function isLiveRunStatus(status) {
  return APB_LIVE_STATES.has(status);
}

/** Map an apb run status to a POSIX-ish exit code for Paperclip. */
export function exitCodeForRunStatus(status) {
  switch (status) {
    case 'succeeded':
      return 0;
    case 'failed':
      return 1;
    case 'aborted':
      return 130; // operator/engine stopped the run
    case 'interrupted':
      return 137; // driver died mid-run
    case 'paused':
      return 75; // EX_TEMPFAIL: waiting on a human gate, retryable later
    default:
      return 1;
  }
}
