# Probe: `gh issue view` empty stdout with exit 0 inside spawned agents

Date: 2026-07-21
Branch: `fix4042/gh-probe`
Issue: #42 finding 12

## Symptom (field report)

Inside spawned node agents (headless, non-TTY), `gh issue view <n>` returned
empty stdout with exit code 0, while `gh api` calls worked. Observed across
several nodes in different runs. Hypothesis: pager / TTY detection in the
spawn environment (`PAGER`, `GH_PAGER`, or gh config `pager`).

## Engine spawn environment (source of truth)

Node agents are spawned in `crates/apb-engine/src/adapter.rs`:

- `Command::new(program)` with `stdout(Stdio::piped())`, `stderr(Stdio::piped())`
- `stdin` is `Stdio::null()` or a short-lived pipe that is closed after the
  prompt is written (agent sees EOF)
- Env mutations before spawn:
  - `ConnectorEnvPolicy::apply`: scrub connector secrets; optionally set
    `APB_RUN_DIR` / `APB_NODE_ID`
  - `apply_agent_home`: for codex only, set isolated `CODEX_HOME`
- Process group isolation via `spawn_in_group` (Unix `process_group(0)`)
- No prior `GH_PAGER` / `PAGER` injection (before this fix)
- Child inherits the rest of the parent process environment

Supervisor spawn uses the same `ConnectorEnvPolicy::apply` path; ACP transport
uses the same env apply as headless.

## Host baseline

- `gh version 2.96.0 (2026-07-02)`
- `gh config get pager` -> empty (`pager:` blank in `~/.config/gh/config.yml`)
- Interactive shell often has `PAGER=cat` / `GH_PAGER=cat` already; login shells
  without that session overlay can leave both unset. Codex shell snapshots on
  this machine also show `export PAGER=less` in some profiles while
  `GH_PAGER=cat` appears in others.

Probe target: `gh issue view 42 -R itechmeat/agentic-playbooks` (a long body).

## Experiments

All non-TTY cases used `stdin=DEVNULL`, `stdout=PIPE` (matching the engine).

### A. Piped stdout (true non-TTY)

| Environment | Result |
| --- | --- |
| no pager env | exit 0, ~7795 bytes body |
| `PAGER=cat` / `GH_PAGER=cat` | exit 0, full body |
| `PAGER=less` / `GH_PAGER=less` | exit 0, full body |
| `PAGER=true` / `GH_PAGER=true` | exit 0, full body |
| `env -i PATH HOME` (minimal) | exit 0, full body |
| `env -i` + `PAGER=less` / `GH_PAGER=less` | exit 0, full body |
| `gh api ... --jq .title` under same | exit 0, title text |

Conclusion for pure pipes: modern `gh` does not invoke the pager when stdout is
not a TTY. Empty-with-exit-0 cannot be reproduced by pager settings alone under
engine-like stdio (piped, non-TTY).

### B. Forced TTY detection (`GH_FORCE_TTY=1`) with piped stdout

| Environment | Result |
| --- | --- |
| `GH_FORCE_TTY=1` alone | exit 0, full body (with ANSI) |
| `GH_FORCE_TTY=1 GH_PAGER=cat` | exit 0, full body |
| `GH_FORCE_TTY=1 GH_PAGER=true` | **exit 0, stdout EMPTY** |
| `GH_FORCE_TTY=1 PAGER=true` | **exit 0, stdout EMPTY** |
| `GH_FORCE_TTY=1 GH_PAGER=less` | exit 0, body present (less pass-through) |
| `GH_FORCE_TTY=1 GH_PAGER=true` + `gh api --jq` | **exit 0, stdout EMPTY** |
| `GH_FORCE_TTY=1 GH_PAGER=cat` + `gh api --jq` | exit 0, body present |

Exact empty-with-exit-0 condition under pipes:

```text
GH_FORCE_TTY is set (any truthy width) AND GH_PAGER or PAGER is a
discard-and-succeed command (for example `true`)
```

Fix under the same conditions:

```text
GH_PAGER=cat  (restores full body; exit 0)
```

### C. Real PTY (what agent shell tools often allocate)

| Environment | Result |
| --- | --- |
| default / `GH_PAGER=cat` | full body after spinner ANSI |
| `GH_PAGER=true` / `PAGER=true` | exit 0; clean content empty (spinner only) |
| `GH_PAGER=less` on long `issue view` | hangs in less (needs keypress); agent-side timeouts look like partial/empty |
| short `gh api --jq` under `GH_PAGER=less` | often exits; longer `api` raw / issue view hang |

So under a PTY, a discard pager yields the field symptom (empty useful output,
exit 0). An interactive pager yields hang / truncated capture rather than a
clean empty exit 0.

### D. Config-only pager via `GH_CONFIG_DIR`

Isolating config into a temp dir without copying `hosts.yml` made auth fail
(exit 4), so config-pager-only cases were not fully isolated from host auth.
Host config has blank `pager:`, so config is not the host default path for this
machine.

## Root condition

Empty stdout with exit 0 happens when **gh believes it should page** and the
selected pager **consumes stdin and writes nothing**, then exits 0.

Ways gh decides to page:

1. Real TTY / PTY on stdout (common when coding agents allocate a pseudo-TTY
   for shell tools, even though the agent process itself was spawned with
   piped stdio by apb).
2. `GH_FORCE_TTY` set in the environment.

Pager selection order (host-relevant): `GH_PAGER` > config `pager` > `PAGER` >
default `less`.

Under pure non-TTY pipes with current gh 2.96.0, the pager is skipped, so the
engine's own stdio wiring alone does not trigger the bug. The field report's
"non-TTY" likely describes the agent process, not every tool subprocess the
agent later starts. The environmental hazard remains: inherited interactive
pager settings are unsafe for headless agent trees.

`gh api` is not inherently immune: under `GH_FORCE_TTY` + `GH_PAGER=true` it
also goes empty. Field cases where only porcelain failed are consistent with a
PTY + long paged body (`issue view`) vs short non-paged / auto-exit paths for
small `api --jq` payloads under `less`.

## Engine-side fix

At the single child-env apply site used by every agent spawn
(`ConnectorEnvPolicy::apply` in `adapter.rs`), force:

```text
GH_PAGER=cat
PAGER=cat
```

One-line rationale in code: headless agents must not inherit an interactive
pager path that can swallow CLI porcelain stdout with exit 0.

Verified locally: `GH_FORCE_TTY=1 GH_PAGER=true` empty case becomes full body
once `GH_PAGER=cat` is set. Unit test spawns `env` under the default policy and
asserts both variables are present (no network).

## What was not changed

- No changes to `agent_home.rs`, stall, or liveness.
- No gh config writes on the host.
- No playbook / prompt workarounds.

## Commands worth re-running

```bash
# Empty-with-0 reproduction (network)
env -u PAGER GH_FORCE_TTY=1 GH_PAGER=true \
  gh issue view 42 -R itechmeat/agentic-playbooks </dev/null | wc -c
# expect 0 stdout bytes, exit 0

# Fix
env -u PAGER GH_FORCE_TTY=1 GH_PAGER=cat \
  gh issue view 42 -R itechmeat/agentic-playbooks </dev/null | wc -c
# expect full body

# Engine unit test (offline)
cargo test -p apb-engine connector_env_policy_forces_pass_through_pager
```
