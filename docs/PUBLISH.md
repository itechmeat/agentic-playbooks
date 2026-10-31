# Publishing: the publish step

A `publish` node pushes the commits a node of the run made to a remote, and
only after a person consented to the exact plan: the URL, the branch, the
commits and what the remote holds now. A push whose result nobody can tell
is never repeated on its own; a person resolves it.

The rule behind it is
[decision record 0010](adr/0010-a-push-leaves-only-through-the-publish-step.md).
How much it guarantees depends on the isolation of the run's other nodes:
see "What holds, honestly" below.

## Declaring it

```yaml
publish_only: true            # optional, playbook level (see below)
nodes:
  - id: implement
    type: agent_task
    prompt: "Implement the change and commit it"
    isolation: full
  - id: publish
    type: publish
    from: implement           # an agent_task or script node, not a list node
    remote: origin            # a remote of the repository
    branch: "apb/{{run.id}}"  # optional; default: the branch `from` committed on
    timeout_seconds: 300      # the push; default 300, 1 to 3600
    prompt: "Check the subject lines"   # optional guidance, shown literally
```

| Field | Meaning |
|---|---|
| `from` | The node whose commits are published. Its latest `artifacts_committed` (since the publish node last succeeded) gives `after`, the commit pushed. |
| `remote` | A remote of the source node's repository, by name. Its push URL is pinned before the first acting node of the run starts, so an agent cannot point it elsewhere. |
| `branch` | The remote branch. A template may read `params.*`, `run.id` and `nodes.<id>.output[.<field>]`. Without it, the branch `from` committed on. |
| `timeout_seconds` | How long the push may take before it is stopped and settled `unknown`. |
| `prompt` | Shown to the person with the plan, never interpreted. |

A publish node implies the effects `network`, `external` and
`irreversible`. So a playbook with one also needs the start consent of every
irreversible playbook ("this run may publish"), a triggered start of it is
refused, and an automatic review decision upstream of it is refused (V73).
The step's own consent covers "publish exactly this".

## The flow

1. **Pin.** Before the first acting node runs, apb reads the remote's push
   URL and the repository's common directory in the tree the plan will
   read (the `from` node's `workdir`, else the run's tree) and journals
   `publish_target_pinned`. A missing remote, a refused URL, a tree that is
   not a repository or a source `workdir` that cannot be resolved yet
   records a refusal instead of failing the run; the plan then refuses. A
   publish node a supervisor patch added later has no pin and refuses
   `no_pin`: its target was not fixed before agents ran. So does a publish
   node of a sub-playbook started after its parent already ran a node that
   can act.
2. **Plan (read-only).** When the run reaches the node, apb checks that the
   tree's repository is the pinned one, the push URL is still the pinned
   one and no `url.*.insteadOf` or `url.*.pushInsteadOf` rule of the
   operator's git config rewrites it, a `file` remote does not lie inside
   the tree's repository or a `writable` path of a node, `HEAD` is `after`
   on the branch `from` committed on, the branch renders a valid name, and
   the remote branch is absent or `after` is a fast-forward of it. It lists
   the remote (`ls-remote`), the commits the push carries (at most 50
   named, the rest counted, with how many of all of them this run made),
   marks each listed one made by this run, and sets
   `outside_push_suspected` when commits of this run are already on the
   remote. The plan is journaled as `publish_planned` with its digest and
   nonce (`publish-` plus 32 hex), then the node asks.
3. **Consent.** The request goes through the review channel with the
   options `publish` and `skip`. A `publish` decision must carry the plan's
   nonce. No recommendation is asked for and nothing is ever decided
   automatically. A supervisor `node_retry` or `run_continue_from` withdraws
   the request; reaching the node again plans and asks anew.
4. **Re-plan, record, push.** On `publish` apb plans again at once. A plan
   whose digest changed (the remote moved, `HEAD` moved) is asked about
   again; nothing is pushed on the old consent. An identical plan is recorded
   in the send ledger (connector `@git`, function `push`, label
   `ref:<branch>`, mode `ref`) and then pushed with an exact lease:
   `--force-with-lease=refs/heads/<branch>:<remote state the person saw>`.
5. **Settle.** The push's porcelain output settles the intent: a ref line
   that updated (or was already up to date) is `confirmed`; `[rejected]` or
   `[remote rejected]` is `not_sent`; anything else (a timeout, an abort, no
   ref line, a remote that failed to report status) is `unknown`.
6. **Re-entering the node** (a resume, a supervisor retry) reads the ledger
   first: a confirmed push finishes `published`; an unknown one is
   reconciled by listing the remote ref, and confirmed (source `reconcile`)
   only when the remote holds `after`; otherwise the node fails again with
   `publish_unknown` and no push is made. A push whose recording apb ended
   while it ran (a crash) may still be on its way, since it runs in its own
   session: it stays pending until its deadline (the node's timeout) before
   the remote is read back. An intent a person settled `not_sent` spends
   the earlier consent: the node plans and asks again.

A pause never interrupts a push in flight (only an abort kills it, and a
killed push is `unknown`). There is no `max_retries` for a publish node and
the attempt-level infrastructure retries never see it.

### The push command

The push and the remote listings are the only git commands apb runs with a
transport and the operator's credentials. They run from a shadow git
directory: a bare repository with an empty template that apb makes for
each plan, push and reconcile in a new directory
`runs/<id>/publish/shadow-<random>` (mode 0700, created exclusively, so a
directory planted there ahead of time is never used) and removes
afterwards. It reaches the tree's objects through its
`objects/info/alternates`. The tree's local config is never read, so its
`core.sshCommand`, `credential.helper`, `url.*.insteadOf`,
`url.*.pushInsteadOf`, `remote.*.receivepack` and hooks cannot act. The
operator's global and system config (their credential helper, their ssh
setup) still apply. Their URL rewrites are weighed the way git picks them
(the longest matching prefix; a push takes the `pushInsteadOf` winner when
one matches, else the `insteadOf` one): when the listing's or the push's
effective URL is not the pinned one, the plan refuses (`url_rewritten`)
rather than sending anything elsewhere. A rule that keeps the URL as it is
changes nothing.

On top of apb's hardened git settings (decision record 0009), minus the
credential helper reset, the command allows exactly one transport (the
URL's own scheme) and sets `http.followRedirects=false`,
`push.gpgSign=false`, `push.useForceIfIncludes=false`,
`push.recurseSubmodules=no`, `submodule.recurse=false` and
`credential.interactive=false`. Every `GIT_*` variable and `SSH_ASKPASS`
are removed, then `GIT_TERMINAL_PROMPT=0`, `GIT_NO_REPLACE_OBJECTS=1`,
`GCM_INTERACTIVE=never` and `SSH_ASKPASS_REQUIRE=never` are set, stdin is
empty and the process runs in its own session, so nothing prompts. The push
is `push --porcelain --no-verify --no-follow-tags --no-signed` with the
exact lease above and the refspec `<after>:refs/heads/<branch>`.

### Accepted URLs

`https`, `ssh` (also the scp-like `host:path`) and `file` with an absolute
path (`file:///srv/r.git`, or a bare absolute path). `http`, `git://`,
remote helpers (`ext::`, `fd::`) and relative paths are refused, a
`file://host/path` among them: git reads everything after `file://` as the
path. A URL carrying credentials is refused: any user info,
query or fragment in an `https` URL (a token can sit in the user name or
a signed query), a password in an `ssh` URL, user info with a `:` in a
scp-like URL (git would read `user:secret@host:path` as the host `user`;
a path like `host:team@org.git` reads the same and is refused too, write
it as `ssh://host/team@org.git`),
or anything the token-shape detector flags. A host or user led by `-`
(ssh would read it as an option) is refused too. A refused URL is never
journaled; an accepted one is safe to journal and show.

## Node outputs

| Output | Status | Meaning |
|---|---|---|
| `published` | succeeded | The remote branch points at `after` |
| `skipped` | succeeded | A person declined; nothing was pushed |
| `nothing_to_publish` | succeeded | `from` recorded no commit and `HEAD` is already on the remote |
| `already_on_remote: ...` | succeeded | The remote branch already points at `after`; nothing was pushed |
| `publish_refused: <code>: ...` | failed | The plan could not be offered (below); nothing in the ledger |
| `publish_not_sent: ...` | failed | The remote refused before updating (a failed lease included) |
| `publish_unknown: ...` | failed | Whether the remote updated cannot be told; names `apb intent resolve` |

### Refusal codes

| Code | Sentence |
|---|---|
| `no_pin` | The remote was not pinned before agents ran, so the target cannot be trusted. |
| `not_a_repository` | The working tree is not a git repository apb runs git in. |
| `remote_missing` | The repository has no such remote. |
| `url_refused` | The remote URL is refused: only https, ssh and file URLs without credentials are pushed to. |
| `remote_changed` | The remote URL changed after it was pinned. |
| `repository_changed` | The working tree belongs to another repository than the one pinned. |
| `head_moved` | HEAD is not the commit the source node recorded. |
| `branch_moved` | HEAD is no longer on the branch the source node committed on. |
| `branch_invalid` | The branch is not a valid branch name. |
| `untracked_commits` | HEAD has commits no node of this run recorded; publish from a node that commits. |
| `not_fast_forward` | The remote branch has commits this tree does not have; integrate them first. |
| `remote_unreachable` | The remote could not be listed. |
| `url_rewritten` | The git configuration rewrites the remote URL to another address. |
| `remote_writable` | The file remote lies inside the working tree or a path a node may write, so its hooks are not the operator's. Also when its path cannot be resolved, and every file remote when a `~` writable path has no home to resolve against. |
| `history_unreadable` | The commits the push would carry could not be read from the tree. |
| `unrecognized` | The run recorded a refusal this apb does not know (the detail names its code). |

## Deciding

Every surface posts through one check (`apb_engine::review::post_decision`):
the decision must be `publish` or `skip`; `publish` needs the nonce of the
open plan; a stale nonce is a conflict that names the current plan; a second
decision for the same plan is refused; a `skip` without a nonce is stamped
with the open plan's. A `publish` from inside a run step (`APB_RUN_ID` or
`APB_HOST_TASK` set) is refused, even when the run's snapshot cannot be
read. A decision for a publish node whose journal or review channel cannot
be read right now is a conflict to retry (it could not be stamped with the
plan's nonce). The nonce names the plan, not the request: a decision
queued just before a supervisor withdrew the request answers a later plan
only when it is identical, the push the person consented to.

**CLI**

- `apb publish show <run> [--node <id>] [--json]` prints the target, the
  plan (what is pushed to which branch of which URL, the remote's current
  state, the commits with the tags `this run` or `before the run` and their
  `Apb-Run:` trailers, the omitted count, the outside-push warning), and,
  while a consent is awaited, the nonce with the exact approve and skip
  commands. After the push it prints the intent and its state, and for an
  unknown one the `apb intent resolve` command.
- `apb publish approve <run> [--node <id>] --nonce <nonce> [--note <text>]`
  and `apb publish skip <run> [--node <id>] [--note <text>]`. `--node` may
  be left out when the run has one publish node, or one that waits; when
  given, it must name a publish node of the run.
- `apb review <run> <node> --decision publish --nonce <nonce>` does the
  same through the general command.
- `apb runs <id>` prints one `publish` line per fact of each publish node:
  the target, the plan, a pending consent, the push and its state, the
  result.
- An unknown push is listed by `apb intent list <run>` and resolved by
  `apb intent resolve <run> <intent> --sent|--not-sent` after checking the
  remote.

**MCP**

- `review_decide` takes `nonce`. The tool description tells the host to
  show the person the plan first and to pass the nonce only after they saw
  it and agreed.
- `pending_review` (in `run_status`, `run_wait` and `supervisor_wait_event`)
  carries `publish: { url, branch, after, remote_before, commits, omitted,
  this_run, outside_push_suspected, plan_digest, nonce }` for a publish
  gate, and `run_wait`'s `next` says to relay that plan before deciding.
- `run_status` and `run_report` carry a `publish` block: per node `node`,
  `remote`, `url`, `pin_refusal`, `plan` (the latest), `awaiting_consent`,
  `push` (the latest: `intent`, `state`, the `after`, `branch`, `url`,
  `remote_before` and `plan_digest` of the plan that push carried, which
  may be an earlier visit's than `plan`, `detail`, `effect_ref`) and
  `result`.

**HTTP**

`POST /api/runs/{id}/review` takes `nonce`. A dashboard without keys
refuses a `publish` decision with `403`, naming the `apb publish approve`
command: anything on the machine reaches a keyless dashboard, a sandboxed
agent under `network: open` included, and the open plan's nonce is in the
run payload. A `skip` still goes through. With keys, a missing nonce or a
decision the gate does not offer is `400`, a stale nonce or a second
decision `409` (the body names the current plan), a node that is not a
gate `404`. The run payload carries `publish` and the plan in
`progress.pending_reviews`. There is no separate route.

**Dashboard**

The run page shows a Publish panel for a publish gate: the node's guidance,
the exact plan, the commit list with its tags, the remote state, and the
buttons "Publish exactly this" (posts the plan's nonce) and "Skip". A
dashboard without keys shows the `apb publish approve` command in place of
the first button. After the push it shows what that push sent, the result
and the intent state; an unknown result reads "Needs a look" with the
resolve command. The node form edits `from` (a
choice of the eligible agent tasks and scripts), `remote`, `branch`, the
push timeout and the guidance, and shows `publish_only` read-only.

## Validation

| Code | Severity | Rule |
|---|---|---|
| V116 | error | `from` names an existing `agent_task` or `script` node, not a list node, from which the publish node is reachable |
| V117 | error | `remote` is a remote name (`[A-Za-z0-9._-]`, not led by `-`); `branch` reads only `params.*`, `run.id` and `nodes.<id>.output[.<field>]` (not a report or a review decision), its literal parts make a valid branch name, not `HEAD`; `timeout_seconds` is 1 to 3600 |
| V118 | error | A publish node does not take `list`, `fork`, `cache`, `success_check`, `route`, `completion_check`, `option_descriptions`, `auto_decide_ok` or `inputs`/`outputs` (`auto_decide` is V72) |
| V119 | warning | The playbook publishes and a node runs an agent without `isolation: full`, or scripts run outside the sandbox (script nodes, baseline script and project-check sources, goal criteria checked by a script): they can push by themselves with the operator's credentials. A sub-playbook node is named too, since its nodes are not checked here |
| V120 | error | `publish_only: true` and a node that runs an agent without `isolation: full` (an agent task, a finish with a prompt, a judge that emulates; the message names the fix); a warning when `publish_only` is set without a publish node |

V90 never fires for a publish node, and V73 counts one downstream of a gate
as irreversible.

## `publish_only`

`publish_only: true` makes V119's agent condition an error (V120): every
node that runs an agent must be `isolation: full`. At attempt time a
credential-shaped variable the profile's allowlist would pass to such an
agent (the limit `credentials.env_passed:<NAME>`) refuses the attempt with
the reason `credentials_passed`. Host mode is already refused by `full`.
Scripts are the author's own code and run outside the sandbox: V119 names
them as a warning, never an error.

A sub-playbook of a run held to `publish_only` is held to it too, whatever
its own playbook says: its run refuses to start unless every one of its
agent nodes is `isolation: full` (the playbook node fails with the names),
and its attempts refuse a passed credential the same way. A sub-playbook
started after its parent ran a node that can act pins nothing, so its own
publish nodes refuse `no_pin`.

## What holds, honestly

| | `full` isolation on every agent node, with `publish_only` | `best_effort` without a sandbox, `none`, host mode, scripts |
|---|---|---|
| What apb pushes is exactly the consented plan, to the pinned URL, under an exact lease | Yes | Yes |
| The push is recorded before it leaves and an unknown one is never repeated on its own | Yes | Yes |
| The tree's config and hooks, and a shadow planted ahead of time, never act on apb's push | Yes | Yes |
| Commits of the run already on the remote are flagged (`outside_push_suspected`) | Yes | Yes |
| Only a person consents: no step of the run can approve its own push | Yes over the CLI and a dashboard with keys (the agent has no `.apb/` and no key); a dashboard without keys refuses a publish consent; over MCP it is an instruction to the host (see Limits) | No: such a step has the operator's environment and can run `apb publish approve` itself |
| Nothing else of the run pushed | Yes, apart from the limits below: the agent has no `SSH_AUTH_SOCK`, no `~/.ssh`, `~/.git-credentials` or `~/.config/gh`, no credential variable (the allowlist is forced and `publish_only` refuses one it would pass), no write access to `.git/config` or `.git/hooks`, and no `.apb/` (so not the shadow or the journal); a sub-playbook's agents are held to the same | No: the agent or script has the operator's environment and home and can push by itself. apb only flags it afterwards |

Limits:

- An agent under `network: open` can still reach an endpoint that needs no
  credentials, and every service listening on this machine's loopback. A
  dashboard without keys refuses it a publish consent but takes its other
  requests; issue a key to close those too.
- A `file` remote inside the tree's repository or a node's `writable` path
  is refused, since an agent could plant hooks there. Any other `file`
  remote's own hooks run, as on any server.
- `GCM_INTERACTIVE=never` is not verified on macOS: a credential helper
  such as `osxkeychain` may still show a dialog there. The push timeout
  bounds the wait.
- A consent has no time-to-live: a plan whose digest is identical
  (`remote_before` included) is the same action.
- Over MCP the server cannot prove a person decided rather than the host
  agent relaying. Like `review_decide` for a gate and `run_intent_resolve`,
  this is an instruction to the host, not an enforcement.

## Not in scope

| Deferred | Reason |
|---|---|
| Force or non-fast-forward publishing | Needs its own consent wording and history-loss warnings |
| Fetching into the shadow to explain a diverged remote | Adds a third networked command; the plan refuses `not_fast_forward` and says to integrate first |
| Tags, several refs, atomic multi-ref pushes | One ref per node keeps the ledger identity simple (`ref:<branch>`) |
| Creating a pull request | A connector function flagged `irreversible` in a later node, which the send ledger already covers |
| A consent time-to-live | An identical plan is the same action; revisit if needed |
| Proving over MCP that a person decided | Same instruction-level rule as `review_decide` and `run_intent_resolve` |
| Watching for an agent's own push during the run | Noisy, since a fetch moves `refs/remotes/*` too; the plan flags `outside_push_suspected` afterwards |
| A start-consent opt-out | One rule first |
| A network limited to the provider's ports (Landlock TCP rules) | Agents reach other ports for tests and tools, and a port list still leaves every host on the provider's port open; a dashboard without keys refuses a publish consent instead |
| A dashboard editor for `publish_only` | Shown read-only |
