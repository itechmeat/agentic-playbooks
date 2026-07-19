// Form generation and result rendering for the connector playground panel
// (spec 2026-07-19-official-connectors-design section 7). Pure functions
// only, no DOM - the panel component (ConnectorPlaygroundPanel.svelte)
// stays thin and calls into this module, mirroring connectorstats.ts.

export interface PlayCallError {
  code: string
  message: string
  http_status?: number
  retry_after_sec?: number
}

// The executor's structured outcome, returned verbatim by POST
// /api/connectors/{name}/call (like HealthcheckResult, no camelCase
// mapping - this is a passthrough JSON blob, not a fetch-boundary DTO).
export interface PlayCallResult {
  ok: boolean
  status?: number
  body?: unknown
  truncated?: boolean
  link?: string | null
  picked?: boolean
  dry_run?: boolean
  method?: string
  url?: string
  error?: PlayCallError
}
