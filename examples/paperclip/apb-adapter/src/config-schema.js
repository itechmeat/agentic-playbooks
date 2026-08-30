export const DEFAULT_APB_BASE_URL = 'http://127.0.0.1:7321';
// Deliberately NO default project: the previous default named a live business
// project, so a blank/typo'd `project` silently dispatched real playbooks.
// A missing project is now a hard config error.
export const DEFAULT_TIMEOUT_MS = 900_000; // 15 min
export const DEFAULT_POLL_INTERVAL_MS = 2_000;
export const MIN_POLL_INTERVAL_MS = 250;
export const DEFAULT_POLL_GIVE_UP_MS = 60_000;

/**
 * Declarative config schema rendered by the Paperclip agent-config UI.
 * Shape: AdapterConfigSchema from @paperclipai/adapter-utils.
 */
export function getConfigSchema() {
  return {
    fields: [
      {
        key: 'apbBaseUrl',
        label: 'apb base URL',
        type: 'text',
        default: DEFAULT_APB_BASE_URL,
        hint: 'Base URL of the apb dashboard/API, e.g. http://127.0.0.1:7321. Loopback HTTP is expected; a non-loopback apb should be fronted by HTTPS.',
      },
      {
        key: 'apbApiKey',
        label: 'apb API key',
        type: 'text',
        hint: 'Only needed when apb runs in server mode with issued keys (apb server key issue). Sent as Authorization: Bearer. Leave empty for the default loopback bind with no keys.',
        meta: { secret: true },
      },
      {
        key: 'project',
        label: 'apb project',
        type: 'text',
        required: true,
        hint: 'REQUIRED. Project name as shown by `apb projects list` / GET /api/projects, resolved to a workspace_id at run time. There is no default: an unset project is a configuration error, so a typo can never silently dispatch into another project.',
      },
      {
        key: 'playbook',
        label: 'Default playbook',
        type: 'text',
        hint: 'Playbook id used when nothing more specific resolves. See the README for the full resolution order.',
      },
      {
        key: 'playbookMap',
        label: 'Playbook map',
        type: 'textarea',
        hint: 'Optional JSON object mapping a Paperclip taskKey, issue id or wakeReason to a playbook id. Keys may be exact, a "PREFIX-*" glob, or "default". Example: {"SUP-*":"wa-intake","default":"docs-check"}',
      },
      {
        key: 'params',
        label: 'Default run params',
        type: 'textarea',
        hint: 'Optional JSON object of apb run params. apb types params as string->string, so values are coerced to strings.',
      },
      {
        key: 'instruction',
        label: 'Default instruction',
        type: 'textarea',
        hint: 'Optional free-text instruction passed to the apb run. Falls back to the Paperclip task text.',
      },
      {
        key: 'timeoutMs',
        label: 'Run timeout (ms)',
        type: 'number',
        default: DEFAULT_TIMEOUT_MS,
        hint: 'How long to wait for the apb run to reach a terminal state. apb exposes no HTTP stop endpoint, so on timeout the run keeps going server-side and must be stopped with `apb stop <run-id>`.',
      },
      {
        key: 'pollIntervalMs',
        label: 'Poll interval (ms)',
        type: 'number',
        default: DEFAULT_POLL_INTERVAL_MS,
        hint: 'How often to re-read the apb run detail. apb has no incremental log endpoint, so each poll refetches the run and the adapter streams only newly-seen events.',
      },
      {
        key: 'onPause',
        label: 'When the apb run pauses',
        type: 'select',
        default: 'return',
        options: [
          { value: 'return', label: 'Return control to Paperclip' },
          { value: 'wait', label: 'Keep waiting until timeout' },
        ],
        hint: 'apb pauses on a human_review / interactive gate. "return" ends the Paperclip run promptly with exit code 75 so the wake is not held open; the apb run stays parked and resumable.',
      },
      {
        key: 'allowTextDirectives',
        label: 'Allow apb: directives in wake text',
        type: 'toggle',
        default: false,
        hint: 'SECURITY: when on, `apb:playbook=<id>` and `apb:param.k=v` are honoured from issue titles/descriptions/comments - text any issue author controls. That lets them choose the playbook and its params. Leave off unless every issue author is trusted. Directives can never override operator params or write paperclip_* keys.',
      },
      {
        key: 'pollGiveUpMs',
        label: 'Poll give-up window (ms)',
        type: 'number',
        default: DEFAULT_POLL_GIVE_UP_MS,
        hint: 'How long apb may stay continuously unreachable mid-run before the adapter stops waiting. Duration-based so a routine apb restart does not abandon a live run.',
      },
      {
        key: 'logParamValues',
        label: 'Log parameter values',
        type: 'toggle',
        default: false,
        hint: 'Off by default: only parameter KEYS are logged, since values may carry customer data. Even when on, keys matching token/key/secret/password are masked.',
      },
      {
        key: 'streamNodeOutput',
        label: 'Stream node outputs',
        type: 'toggle',
        default: true,
        hint: 'Include apb node outputs in what leaves the adapter. When off, outputs are withheld from the log stream, from onEvent payloads, from resultJson, and from the summary - all four paths, not just the rendered log line.',
      },
    ],
  };
}
