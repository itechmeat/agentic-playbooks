/**
 * Presentation metadata for the node form: a readable label and a one-or-two
 * sentence explanation of what each field actually does.
 *
 * The YAML keys stay what they are (`max_retries`, `success_check`); this is
 * the only place that turns them into something a person reads, so the form
 * never shows a raw schema key as a label. The hints are rendered in the
 * field's info popover and are the closest thing the editor has to inline
 * docs, so they state real engine behavior - including where a field is
 * declared but not yet enforced.
 *
 * Keys are form fields, not schema fields: `prompt` and `finish_prompt` are
 * the same YAML key on two node kinds that mean different things, and
 * `input_draft` is not part of the playbook at all.
 */
export interface NodeFieldInfo {
  /** Label shown above the control. */
  label: string
  /** What the field does, shown in the info popover. */
  hint: string
}

export const NODE_FIELDS = {
  title: {
    label: 'Title',
    hint: 'Name shown on the graph and in run progress. Optional: while it is empty the node id is displayed instead, which is why a node can look named without carrying a title.',
  },
  input_draft: {
    label: 'Input prompt',
    hint: 'Starting instruction handed to the next run of this playbook. It is stored next to the playbook rather than inside it, so it is not versioned and changing it does not create a new version.',
  },
  prompt: {
    label: 'Prompt',
    hint: 'Instruction sent to the agent. Rendered as a template first, so it can pull in {{ params.name }}, {{ run.instruction }} and the output of an earlier node as {{ nodes.<id>.output }}.',
  },
  profile: {
    label: 'Profile',
    hint: 'The executor binding: which agent and model run this node, plus fallbacks, role prompt and skills. Type a bare name to resolve the nearest scope, or scope/name (project/... or global/...) to pin one. Empty falls back to the playbook default profile.',
  },
  isolation: {
    label: 'Isolation',
    hint: 'How far the node should run apart from the working copy: none is the shared directory, best_effort asks the adapter for as much isolation as it supports, full demands a fully isolated sandbox. Declarative for now - the engine records the requirement and the validator warns, but it does not yet enforce it.',
  },
  max_retries: {
    label: 'Max retries',
    hint: 'Extra attempts after a failed one before the node is final. Empty inherits the playbook default, which is 0 - one attempt and no retry.',
  },
  timeout_seconds: {
    label: 'Timeout, seconds',
    hint: 'Wall-clock limit for a single attempt. When it expires the attempt is cancelled and counts as a failure, so a retry budget still applies. Empty inherits the playbook default, which is no limit.',
  },
  success_check: {
    label: 'Success check',
    hint: 'An extra gate on top of what the agent reports about itself. Script path: a sh script under the version scripts/ directory that must exit 0. Completion marker: a literal string that must appear in the node output, which catches an agent that stops early and reports interim work as done.',
  },
  connectors: {
    label: 'Connectors',
    hint: 'External services this node is allowed to call, and which accounts and functions of each. Anything left unchecked is not granted. max_calls caps how many calls to that connector one run may make.',
  },
  max_calls: {
    label: 'Max calls',
    hint: 'Upper bound on calls to this connector within one run. Empty means no cap.',
  },
  runner: {
    label: 'Runner',
    hint: 'The interpreter that executes the script, for example sh or python3.',
  },
  script: {
    label: 'Script',
    hint: 'Path to the script to run, relative to the playbook version scripts/ directory.',
  },
  max_loops: {
    label: 'Max loops',
    hint: 'How many times a cycle through this condition may repeat before the run stops. Any cycle in the graph needs a bound, either here or as max_traversals on one of its edges, otherwise the playbook does not validate.',
  },
  outcome: {
    label: 'Outcome',
    hint: 'The verdict this end of the graph records for the run: success or failure.',
  },
  finish_prompt: {
    label: 'Prompt',
    hint: 'Optional. When set, an agent composes the run answer from the accumulated run context and its output becomes the run result. When empty the run finishes instantly with an empty answer.',
  },
  playbook: {
    label: 'Playbook',
    hint: 'The child playbook this node runs. A bare id resolves in this project first and then globally; scope/id pins one, for example global/child.',
  },
  instruction: {
    label: 'Instruction',
    hint: 'Template rendered with this run context; the result becomes the child run input and overrides whatever draft the child carries. Empty leaves the child on its own draft.',
  },
} as const satisfies Record<string, NodeFieldInfo>

export type NodeFieldKey = keyof typeof NODE_FIELDS
