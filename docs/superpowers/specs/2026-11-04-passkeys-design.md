# Passkeys on person-only gates and approvals

Date: 2026-11-04. Status: accepted. Builds on the approvals work
(`2026-11-03-approvals-design.md`); usage in
`docs/GUARDRAILS.md` ("Passkeys on person-only gates and approvals") and
`docs/MCP.md` ("Approvals and person-only gates").

## 1. Problem

A person-only gate refuses every channel that is not a person's, but a
person's channel is provable by credentials, not by hands: a server key
signs in to the dashboard, so anything on the machine that reads the key
file and the config dir can decide a person-only gate today. The gate
should hold when the deciding process holds no credential that can
produce a WebAuthn assertion.

## 2. Design

### Store

- `<config_dir>/passkeys.yaml`, one entry per passkey: credential id,
  COSE public key, sign count, label, created time. Written 0600,
  atomically, through `apb_core::fsutil` (`passkey.rs` in `apb-core`).
- Nothing secret is stored, and nothing secret is logged: the
  authenticator's private key never leaves the authenticator.
- The enrollment one-time code also lives as a file under the config dir,
  same mode, deleted when used or expired.

### Ceremonies

- apb-server verifies registration and assertion itself
  (`crates/apb-server/src/webauthn.rs`): ES256 and EdDSA, attestation
  `none`, `userVerification` required. No `webauthn-rs` (license);
  verification uses `p256`, `ed25519-dalek`, `ciborium` (MIT or
  Apache-2.0).
- Challenges are single use, held in memory only, and expire after two
  minutes (`crates/apb-server/src/passkeys.rs`).
- Origin and RP ID are checked against the dashboard's own origin:
  `server.public_base_url`, else the request host on http.
- An IP-literal RP ID is refused with a clear message: passkeys cannot
  bind to an IP address, so a dashboard reached at `http://127.0.0.1:port`
  cannot use them.

### Enrollment

- `apb passkey enroll` (`passkey.rs` in `apb-cli`): person only, refused
  inside a run step like the other person-only commands. It prints a
  one-time code, valid five minutes; the dashboard's Passkeys page asks
  for it and runs the registration ceremony.
- Every later passkey is added through the dashboard with an assertion
  from an existing passkey.
- `apb passkey list`; `apb passkey remove <label>`. Removing the LAST
  passkey needs a one-time code typed back at the terminal, so an agent
  channel cannot empty the store.
- Enrollment and removal journal nothing into runs. Each sends a notice
  to the machine's notify target when one is set ("a passkey was added
  on `<machine>`"), so an enrollment a person did not make is visible.

### Deciding

- When at least one passkey exists, three decisions need a fresh
  assertion bound to the run, the gate, the request and the chosen
  option, made through the dashboard: a person-only gate's decision, an
  approval (a gate with `approves` or `requires_approval`), and a
  publish consent.
- The CLI (`apb review`, `apb publish approve`) prints the dashboard link
  and refuses with `passkey_required`, a new code in the closed
  `CallerRefusal` list, rendered by the facades like the others. MCP and
  bearer stay refused as before.
- The engine stamps the credential id prefix (`passkey`) on the consumed
  channel entry, and a person-only or bound gate consumes nothing else
  while a passkey is enrolled. `apb doctor --run` lists an unstamped
  entry as "no passkey assertion".
- Exempt: an unattended run's declared `decide` and a scenario run's
  script. They are not a person's consent anyway, and V156 already
  refuses them on a person-only gate.
- When no passkey exists, behavior is exactly as before. `apb doctor`
  shows a warning row recommending `apb passkey enroll`.

### UI

- A Passkeys page: list, add, remove.
- The decide button on a person-only gate or an approval runs
  `navigator.credentials.get`. On a non-secure context (HTTP on a
  non-loopback name) it shows "Passkeys need https or localhost".

## 3. Threat model

**Asset.** A person's decision on a person-only gate or an approval.

**What the gate buys.** An agent that reads the config dir and the
dashboard key file can sign in to the dashboard but still cannot decide:
producing the assertion needs the authenticator's private key, which is
not on the machine's disk and not in its memory.

| Threat | Control |
|---|---|
| an agent channel or a key holder deciding | `passkey_required`; the dashboard asks for the assertion |
| a replayed assertion | the challenge is single use, two minutes, bound to run, gate, request and option |
| a dashboard at an IP literal | no RP ID possible; refused with a clear message |
| an agent enrolling its own passkey | enroll is person only at a terminal; the notice and `apb passkey list` show it |
| an agent removing every passkey | the last removal needs a code typed back at the terminal |
| a silently emptied store | the gate falls back to the old behavior; the doctor row and the notice say what happened |

**Limits, stated honestly.**

- A same-user agent that drives the person's own browser (for example
  through remote debugging) or a virtual authenticator can sign in and
  produce the assertion. Out of scope.
- Enrollment is the protected step (a person at a terminal); visibility
  of an agent's enrollment is notice plus `apb passkey list`, not a
  refusal.
- The gate still does not prove a person decided; it proves an
  authenticator holding the enrolled key answered within two minutes.

## 4. Deliberately out of scope

- Same-user browser driving and virtual authenticators (above).
- Passkeys per project or per playbook; the store is per machine.
- Roaming between machines; the store is local to the config dir.
- Attestation verification; `attestation: none` only.
- Protecting judge decisions, ordinary gates or questions; only
  person-only gates, approvals and publish consent are bound.
