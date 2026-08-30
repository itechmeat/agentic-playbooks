/**
 * Playbook + parameter resolution for the apb adapter.
 *
 * ---------------------------------------------------------------------------
 * THE REAL PAPERCLIP CONTEXT CONTRACT
 * ---------------------------------------------------------------------------
 * Verified against the installed @paperclipai/server bundle
 * (dist/services/heartbeat.js, 2026.824.1). The free text a wake carries lives
 * at exactly these paths - there is no `taskTitle` / `taskBody` / `context.task`:
 *
 *   context.paperclipWake.issue.{title,description,identifier,status,workMode}
 *                                          (buildPaperclipWakePayload, ~L4138)
 *   context.paperclipWake.agentMessage.text                          (~L4151)
 *   context.paperclipIssue.{title,description,identifier,workMode}   (~L10889)
 *   context.paperclipTaskMarkdown            rendered task brief     (~L10907)
 *   context.paperclipTaskMarkdownCompact     same, without description
 *   context.paperclipWakeComment             the triggering comment
 *
 * Scalars: `context.taskId`, `context.issueId`, `context.wakeReason`.
 * NOTE `taskKey` is NOT on the context bag - it is `ctx.runtime.taskKey`.
 * There is no `context.issueIds` array and no `context.wakeSource`.
 *
 * ---------------------------------------------------------------------------
 * PLAYBOOK RESOLUTION ORDER (first match wins)
 * ---------------------------------------------------------------------------
 *   1. runtime.sessionParams.apbPlaybook   - per-session pin.
 *   2. context.apb.playbook / context.apbPlaybook - structured hint from a
 *      programmatic caller (never populated by Paperclip itself).
 *   3. `apb:playbook=<id>` directive in wake text - ONLY when the operator sets
 *      `allowTextDirectives: true`. See the security note below.
 *   4. adapterConfig.playbookMap - keyed by taskKey, then issue identifier/id,
 *      then wakeReason. Exact keys, then `PREFIX-*` globs, then `default`.
 *   5. adapterConfig.playbook
 *   6. -> error
 *
 * ---------------------------------------------------------------------------
 * SECURITY: why text directives are opt-in and default OFF
 * ---------------------------------------------------------------------------
 * The text scanned above is verbatim, attacker-controllable issue content: any
 * user who can file or comment on an issue writes it. Honouring
 * `apb:playbook=` / `apb:param.<k>=` from that text lets an issue author choose
 * which playbook runs and with which parameters - a straightforward injection
 * channel into the automation engine. It is therefore gated behind
 * `allowTextDirectives` (default false), and even when enabled:
 *   - the separator is mandatory, so prose like "apb:playbooks are neat" cannot
 *     be misread as a directive;
 *   - a directive may never overwrite an operator-supplied param; and
 *   - a directive may never write a `paperclip_*` provenance key.
 */

/** `apb:playbook=<id>` - separator REQUIRED (an optional one matched prose). */
const PLAYBOOK_DIRECTIVE = /\bapb:playbook\s*[=:]\s*([A-Za-z0-9][A-Za-z0-9._-]*)/i;
/** `apb:param.<name>=<value>` - separator likewise required. */
const PARAM_DIRECTIVE = /\bapb:param\.([A-Za-z0-9_][A-Za-z0-9._-]*)\s*[=:]\s*("[^"]*"|'[^']*'|\S+)/gi;

/** Provenance keys the adapter owns; a directive can never write these. */
const RESERVED_PARAM_PREFIX = 'paperclip_';

export class ResolutionError extends Error {
  constructor(message, code = 'APB_RESOLUTION_FAILED') {
    super(message);
    this.name = 'ResolutionError';
    this.code = code;
  }
}

const str = (v) => (typeof v === 'string' && v.trim() ? v.trim() : null);
const obj = (v) => (v && typeof v === 'object' && !Array.isArray(v) ? v : null);

/** The issue attached to this wake, from either of the two places it appears. */
export function readIssue(context = {}) {
  return obj(context.paperclipIssue) ?? obj(obj(context.paperclipWake)?.issue) ?? null;
}

/**
 * All attacker-influenced free text carried by a wake, in the order a human
 * would read it. Used for directive scanning and instruction composition.
 */
export function collectText(context = {}) {
  const parts = [];
  const wake = obj(context.paperclipWake);
  const issue = readIssue(context);

  if (issue) {
    const ident = str(issue.identifier);
    const title = str(issue.title);
    if (title) parts.push(ident ? `${ident}: ${title}` : title);
    const desc = str(issue.description);
    if (desc) parts.push(desc);
  }

  // The rendered task brief already folds in the issue, so prefer it only when
  // no issue object was present (avoids duplicating the same text twice).
  const brief = str(context.paperclipTaskMarkdown) ?? str(context.paperclipTaskMarkdownCompact);
  if (brief && !issue) parts.push(brief);

  const msg = str(obj(wake?.agentMessage)?.text);
  if (msg) parts.push(msg);

  const comment = obj(context.paperclipWakeComment);
  const commentBody = str(comment?.body) ?? str(comment?.text);
  if (commentBody) parts.push(commentBody);

  return parts.join('\n\n');
}

/** Glob-aware lookup: exact key, then `PREFIX-*` patterns, then `default`. */
function lookupMap(map, candidates, warn) {
  if (!map || typeof map !== 'object') return null;

  const usable = (playbook, via) => {
    if (typeof playbook === 'string' && playbook.trim()) return { playbook: playbook.trim(), via };
    // A non-string value silently resolving to "no match" hides a config typo.
    warn?.(`playbookMap entry ${via} has a non-string value (${JSON.stringify(playbook)}) and was ignored`);
    return null;
  };

  for (const c of candidates) {
    if (typeof c !== 'string' || !c) continue;
    if (Object.hasOwn(map, c)) {
      const hit = usable(map[c], `playbookMap["${c}"]`);
      if (hit) return hit;
    }
  }
  for (const [pattern, playbook] of Object.entries(map)) {
    if (!pattern.includes('*')) continue;
    const rx = new RegExp(`^${pattern.split('*').map(escapeRe).join('.*')}$`);
    for (const c of candidates) {
      if (typeof c === 'string' && c && rx.test(c)) {
        const hit = usable(playbook, `playbookMap["${pattern}"] (glob)`);
        if (hit) return hit;
      }
    }
  }
  if (Object.hasOwn(map, 'default')) return usable(map.default, 'playbookMap["default"]');
  return null;
}

function escapeRe(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** Candidate keys a playbookMap may be keyed by, most specific first. */
function mapCandidates(context, taskKey) {
  const issue = readIssue(context);
  return [taskKey, str(issue?.identifier), str(issue?.id), str(context.issueId), str(context.wakeReason)];
}

/**
 * @param {object} input
 * @param {object} input.adapterConfig  normalized config
 * @param {object} input.context        the Paperclip context bag
 * @param {object} input.sessionParams  ctx.runtime.sessionParams
 * @param {string|null} input.taskKey   ctx.runtime.taskKey
 * @param {(msg:string)=>void} [input.warn]
 * @returns {{playbook: string, via: string}}
 */
export function resolvePlaybook({ adapterConfig = {}, context = {}, sessionParams = {}, taskKey = null, warn } = {}) {
  const pin = str(sessionParams?.apbPlaybook);
  if (pin) return { playbook: pin, via: 'runtime.sessionParams.apbPlaybook' };

  const structured = str(obj(context.apb)?.playbook) ?? str(context.apbPlaybook);
  if (structured) return { playbook: structured, via: 'context hint' };

  if (adapterConfig.allowTextDirectives) {
    const directive = PLAYBOOK_DIRECTIVE.exec(collectText(context));
    if (directive) return { playbook: directive[1], via: 'apb:playbook directive (allowTextDirectives)' };
  }

  const mapHit = lookupMap(adapterConfig.playbookMap, mapCandidates(context, taskKey), warn);
  if (mapHit) return mapHit;

  const fallback = str(adapterConfig.playbook);
  if (fallback) return { playbook: fallback, via: 'adapterConfig.playbook' };

  throw new ResolutionError(
    'no apb playbook could be resolved: set adapterConfig.playbook, add a playbookMap entry, ' +
      'or (if you accept the injection risk) enable allowTextDirectives and put `apb:playbook=<id>` in the task text',
    'APB_NO_PLAYBOOK',
  );
}

/**
 * @returns {Record<string,string>} apb run params (all values stringified)
 */
export function resolveParams({ adapterConfig = {}, context = {}, ctx = {}, taskKey = null, warn } = {}) {
  const out = {};
  const operatorKeys = new Set();

  const assign = (source, { operator }) => {
    if (!source || typeof source !== 'object') return;
    for (const [k, v] of Object.entries(source)) {
      if (v === undefined || v === null) continue;
      out[String(k)] = typeof v === 'string' ? v : JSON.stringify(v);
      if (operator) operatorKeys.add(String(k));
    }
  };

  // Operator-supplied values. Directives may never displace these.
  assign(adapterConfig.params, { operator: true });
  assign(obj(context.apb)?.params, { operator: true });

  if (adapterConfig.allowTextDirectives) {
    PARAM_DIRECTIVE.lastIndex = 0;
    let m;
    const text = collectText(context);
    while ((m = PARAM_DIRECTIVE.exec(text)) !== null) {
      const key = m[1];
      if (key.toLowerCase().startsWith(RESERVED_PARAM_PREFIX)) {
        warn?.(`ignored apb:param directive for reserved key "${key}" (provenance keys cannot be set from wake text)`);
        continue;
      }
      if (operatorKeys.has(key)) {
        warn?.(`ignored apb:param directive for "${key}" (an operator-configured value takes precedence)`);
        continue;
      }
      out[key] = m[2].replace(/^["']|["']$/g, '');
    }
  }

  const issue = readIssue(context);
  const provenance = {
    paperclip_run_id: ctx.runId,
    paperclip_agent_id: ctx.agent?.id,
    paperclip_company_id: ctx.agent?.companyId,
    paperclip_task_id: context.taskId,
    paperclip_task_key: taskKey,
    paperclip_wake_reason: context.wakeReason,
    paperclip_issue_id: str(issue?.id) ?? str(context.issueId),
    paperclip_issue_key: str(issue?.identifier),
  };
  for (const [k, v] of Object.entries(provenance)) {
    if (v === undefined || v === null || v === '') continue;
    if (!Object.hasOwn(out, k)) out[k] = String(v);
  }
  return out;
}

/**
 * The free-text `instruction` handed to apb.
 * `context.apb.instruction` > `adapterConfig.instruction` > the real wake text.
 *
 * `wakeReason` is deliberately excluded: it is an internal Paperclip state token
 * (e.g. `finish_successful_run_handoff`) that means nothing to a playbook and
 * only pollutes the prompt. It travels as the `paperclip_wake_reason` param.
 */
export function resolveInstruction({ adapterConfig = {}, context = {}, ctx = {}, taskKey = null } = {}) {
  const explicit = str(obj(context.apb)?.instruction) ?? str(adapterConfig.instruction);
  if (explicit) return explicit;

  const text = collectText(context);
  if (text.trim()) return text.trim();

  const bits = [];
  if (taskKey) bits.push(`task ${taskKey}`);
  if (str(context.taskId)) bits.push(`task id ${context.taskId}`);
  if (ctx.runId) bits.push(`paperclip run ${ctx.runId}`);
  return bits.length ? `Dispatched by Paperclip (${bits.join('; ')}).` : null;
}
