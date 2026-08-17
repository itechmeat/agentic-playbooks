// Connector endpoints, including the wire-shape mapping: the API speaks
// snake_case, the dashboard types are camelCase, and the translation belongs
// at this boundary rather than in the components.

import type {
  ConnectorAccount,
  ConnectorCard,
  ConnectorDetail,
  ConnectorFunction,
  ConnectorMeta,
  ConnectorTrust,
  JsonSchema,
} from '../connectors'
import type { AvailableConnector, InstallResult, UninstallResult } from '../connectorinstall'
import type { ConnectorFunctionStat, ConnectorStats } from '../connectorstats'
import type { PlayCallResult } from '../connectorplay'
import type { ConnectorInbox, ConnectorInboxAccount, InboxEventRow } from '../connectorinbox'
import { getJson, jsonHeaders, qs, requestJson } from './http'

// dashboard types in `./connectors` are camelCase, so the mapping happens
// here, at the fetch boundary, the same way the rest of this file owns the
// wire<->UI shape.
const conn = (name: string) => `/api/connectors/${encodeURIComponent(name)}`

interface ConnectorCardDto {
  name: string
  version: string
  display_name: string
  summary: string
  tags: string[]
  trust: ConnectorTrust
  accounts_total: number
  accounts_ready: number
}

const toConnectorCard = (d: ConnectorCardDto): ConnectorCard => ({
  name: d.name,
  version: d.version,
  displayName: d.display_name,
  summary: d.summary,
  tags: d.tags,
  trust: d.trust,
  accountsTotal: d.accounts_total,
  accountsReady: d.accounts_ready,
})

export const fetchConnectors = (workspace = '') =>
  getJson<ConnectorCardDto[]>(`/api/connectors${qs({ workspace })}`).then((list) =>
    list.map(toConnectorCard),
  )

interface ConnectorAccountDto {
  name: string
  default: boolean
  fields: Record<string, string>
  missing_env: string[]
  trust: ConnectorTrust
}

const toConnectorAccount = (d: ConnectorAccountDto): ConnectorAccount => ({
  name: d.name,
  default: d.default,
  fields: d.fields,
  missingEnv: d.missing_env,
  trust: d.trust,
})

interface ConnectorFunctionDto {
  name: string
  description: string
  read_only: boolean
  // The manifest's optional deprecation reason. Absent and null both mean
  // "not deprecated"; the string is human-readable text, not a flag.
  deprecated?: string | null
  args_schema?: JsonSchema | null
}

const toConnectorFunction = (d: ConnectorFunctionDto): ConnectorFunction => ({
  name: d.name,
  description: d.description,
  readOnly: d.read_only,
  deprecated: d.deprecated ?? null,
  argsSchema: d.args_schema ?? null,
})

interface ConnectorDetailDto {
  name: string
  version: string
  installed: boolean
  trust: ConnectorTrust
  meta: ConnectorMeta
  body_md: string
  functions: ConnectorFunctionDto[]
  accounts: ConnectorAccountDto[]
}

export const fetchConnector = (name: string, workspace = '') =>
  getJson<ConnectorDetailDto>(`${conn(name)}${qs({ workspace })}`).then(
    (d): ConnectorDetail => ({
      name: d.name,
      version: d.version,
      installed: d.installed,
      trust: d.trust,
      meta: d.meta,
      bodyMd: d.body_md,
      functions: d.functions.map(toConnectorFunction),
      accounts: d.accounts.map(toConnectorAccount),
    }),
  )

export interface HealthcheckError {
  code: string
  message: string
  http_status?: number
  retry_after_sec?: number
}
export interface HealthcheckResult {
  ok: boolean
  error?: HealthcheckError
  [key: string]: unknown
}

// The executor's structured outcome, returned verbatim (design doc section
// 9/8). The server answers HTTP 200 even for failures, so a trust-gated
// refusal arrives as a normal `ok:false` body with `error.code === "permission"`,
// never as an HTTP error. requestJson's non-ok branch only fires on
// transport-level or server-level HTTP errors, not on healthcheck outcomes.
export const runConnectorHealthcheck = (name: string, account: string, workspace = '') =>
  requestJson<HealthcheckResult>(
    `${conn(name)}/healthcheck/${encodeURIComponent(account)}${qs({ workspace })}`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify({}) },
  )

interface AvailableConnectorDto {
  name: string
  version: string
  display_name: string
  summary: string
  tags: string[]
}

// GET /api/connectors/available: the embedded official connectors that are NOT
// installed. Always 200; an empty array means everything is already installed.
export const fetchAvailableConnectors = () =>
  getJson<AvailableConnectorDto[]>('/api/connectors/available').then((list) =>
    list.map(
      (d): AvailableConnector => ({
        name: d.name,
        version: d.version,
        displayName: d.display_name,
        summary: d.summary,
        tags: d.tags,
      }),
    ),
  )

interface InstallResultDto {
  ok: boolean
  name: string
  version: string
  digest: string
  no_op: boolean
  trust_recorded: boolean
  trust_warning: string | null
}

// POST /api/connectors/{name}/install: `force` replaces a different installed
// version, which the server otherwise refuses with 409 needs_force. Only ever
// sent as a deliberate user action, never as an automatic retry.
export const installConnector = (name: string, force = false) =>
  requestJson<InstallResultDto>(`${conn(name)}/install${qs({ force: force ? 'true' : '' })}`, {
    method: 'POST',
  }).then(
    (d): InstallResult => ({
      ok: d.ok,
      name: d.name,
      version: d.version,
      digest: d.digest,
      noOp: d.no_op,
      trustRecorded: d.trust_recorded,
      trustWarning: d.trust_warning,
    }),
  )

// POST /api/connectors/{name}/uninstall: removes the connector tree only. The
// account configuration lives in a separate store and is left untouched.
export const uninstallConnector = (name: string) =>
  requestJson<{ ok: boolean; name: string; no_op: boolean }>(`${conn(name)}/uninstall`, {
    method: 'POST',
  }).then((d): UninstallResult => ({ ok: d.ok, name: d.name, noOp: d.no_op }))

export const approveConnector = (name: string, account: string | null = null, workspace = '') =>
  requestJson<{ ok: boolean }>(`/api/connectors/approve${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ name, account }),
  })

interface ConnectorFunctionStatDto {
  function: string
  account: string
  calls: number
  errors: number
  avg_duration_ms: number
}

interface ConnectorStatsDto {
  connector: string
  runs_scanned: number
  calls: number
  by_function: ConnectorFunctionStatDto[]
  by_outcome: Record<string, number>
}

const toConnectorFunctionStat = (d: ConnectorFunctionStatDto): ConnectorFunctionStat => ({
  function: d.function,
  account: d.account,
  calls: d.calls,
  errors: d.errors,
  avgDurationMs: d.avg_duration_ms,
})

// GET /api/connectors/{name}/stats: usage stats aggregated server-side from
// recent run event logs (design doc section 9). Read-only; the server bounds
// the run scan itself, `runsScanned` reports how many it actually read.
export const fetchConnectorStats = (name: string, workspace = '') =>
  getJson<ConnectorStatsDto>(`${conn(name)}/stats${qs({ workspace })}`).then(
    (d): ConnectorStats => ({
      connector: d.connector,
      runsScanned: d.runs_scanned,
      calls: d.calls,
      byFunction: d.by_function.map(toConnectorFunctionStat),
      byOutcome: d.by_outcome,
    }),
  )

export interface PlayCallRequest {
  function: string
  account: string | null
  args: Record<string, unknown>
  dryRun: boolean
  // Bypasses the function's response_pick projection (spec 4.5 / 2026-07-19
  // section 7 post-review fix), mirroring the CLI's --full debugging
  // escape. false (the playground default) applies the projection like a
  // normal agent call, so a projected function's `picked` flag reads true.
  full: boolean
}

interface PlayCallRequestDto {
  function: string
  account: string | null
  args: Record<string, unknown>
  dry_run: boolean
  full: boolean
}

// POST /api/connectors/{name}/call: the dashboard playground's manual call
// (design doc 2026-07-19-official-connectors-design section 7). Wraps the
// same live execution path the healthcheck probe uses, extended with an
// arbitrary function, args, a dry-run flag, and a full flag. Like the
// healthcheck probe, the server answers HTTP 200 even for a refused or
// failed call - the outcome is carried in the body's `ok`/`error`, never as
// an HTTP error.
export const callConnector = (name: string, req: PlayCallRequest, workspace = '') =>
  requestJson<PlayCallResult>(`${conn(name)}/call${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({
      function: req.function,
      account: req.account,
      args: req.args,
      dry_run: req.dryRun,
      full: req.full,
    } satisfies PlayCallRequestDto),
  })

interface ConnectorInboxAccountDto {
  account: string
  pending: number
  total: number
  cursor: number
  last_received_at: number | null
  dropped: number
  callback_url: string | null
}

interface ConnectorInboxDto {
  connector: string
  has_webhook: boolean
  public_base_url_set: boolean
  accounts: ConnectorInboxAccountDto[]
}

const toInboxAccount = (d: ConnectorInboxAccountDto): ConnectorInboxAccount => ({
  account: d.account,
  pending: d.pending,
  total: d.total,
  cursor: d.cursor,
  lastReceivedAt: d.last_received_at,
  callbackUrl: d.callback_url,
  dropped: d.dropped,
})

// GET /api/connectors/{name}/inbox: counts and the callback URL per account.
// Carries no event body and no provider id; the panel asks for those
// separately and only when the operator expands an account.
export const fetchConnectorInbox = (name: string) =>
  getJson<ConnectorInboxDto>(`${conn(name)}/inbox`).then(
    (d): ConnectorInbox => ({
      connector: d.connector,
      hasWebhook: d.has_webhook,
      publicBaseUrlSet: d.public_base_url_set,
      accounts: d.accounts.map(toInboxAccount),
    }),
  )

interface InboxEventDto {
  seq: number
  received_at: number
  body: unknown
}

// GET /api/connectors/{name}/inbox/{account}/events: the stored payloads.
// The one call in the dashboard that returns delivered content, made only on
// an explicit expand, and rendered behind an untrusted-content notice.
export const fetchConnectorInboxEvents = (name: string, account: string, limit = 20) =>
  getJson<{ events: InboxEventDto[] }>(
    `${conn(name)}/inbox/${encodeURIComponent(account)}/events${qs({ limit: String(limit) })}`,
  ).then((d): InboxEventRow[] =>
    d.events.map((e) => ({ seq: e.seq, receivedAt: e.received_at, body: e.body })),
  )
