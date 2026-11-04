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
  inside a run step like the other person-only commands, and only at an
  interactive terminal. It prints a one-time code, valid five minutes,
  and asks the person to type it back; the code is written only after
  that confirmation, so a piped stdout leaks nothing usable. The
  dashboard's Passkeys page asks for the code and runs the registration
  ceremony.
- Every later passkey is added through the dashboard with an assertion
  from an existing passkey.
- `apb passkey list`; `apb passkey remove <label>`. Every removal, last
  passkey or not, needs a one-time code typed back at the terminal (the
  same rule the dashboard enforces with an assertion), and the
  never-the-last rule lives inside the store's lock, so two concurrent
  removals cannot race the count down to zero.
- Enrollment and removal journal nothing into runs. Each sends a notice
  to the machine's notify target when one is set ("a passkey was added
  on `<machine>`"), so an enrollment a person did not make through apb's
  own surfaces is visible. A record appended into `passkeys.yaml`
  directly sends no notice; see the limits below.

### Deciding

- When at least one passkey exists, three decisions need a fresh
  assertion bound to the run, the gate, the request and the chosen
  option, made through the dashboard: a person-only gate's decision, an
  approval (a gate with `approves` or `requires_approval`), and a
  publish consent.
- The CLI (`apb review`, `apb publish approve`) prints the dashboard link
  and refuses with `passkey_required`, a new code in the closed
  `CallerRefusal` list, rendered by the facades like the others. MCP is
  refused as before, and a bearer-key request is refused even when it
  carries a valid token: a key-made client is never a person.
- The assertion challenge for a decision is not random: it is derived
  from the run, the gate, the decision, the request seq and the binding
  nonce (`review::passkey_proof::decision_challenge`). The engine stamps
  the credential id prefix (`passkey`) and the full assertion
  (`passkey_proof`) on the consumed channel entry and re-verifies the
  proof against the store when it consumes the decision, so a hand
  written line with a made-up prefix, or a proof replayed onto another
  request, is refused; `apb doctor --run` names it. What the proof does
  not re-check is the browser's origin (the engine cannot see the
  request's Host); that check happens where the assertion is first
  accepted, in the server.
- A person-only or bound gate consumes nothing else while a passkey is
  enforced, and an unreadable store enforces too (fail closed): every
  decision path refuses until the file is fixed, and `apb doctor`
  reports it as a failure, not a warning.
- Exempt, at consumption as at append: an unattended run's declared
  `decide` and a scenario run's script. They are not a person's consent
  anyway, and V156 already refuses them on a person-only gate.
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
| a forged stamp in the channel file | the entry carries the assertion; the drive re-verifies it at consumption |
| a corrupt or half-written store | fail closed: person-only decisions refuse until the file reads; `apb doctor` reports a failure |

**Limits, stated honestly.**

- A same-user agent that drives the person's own browser (for example
  through remote debugging), a virtual authenticator, or a process that
  captures the enroll code from the terminal pane (piped stdout, a herdr
  or tmux pane) and races the person during the enrollment window can
  produce the assertion. Out of scope.
- Enrollment is the protected step (a person at a terminal); visibility
  of an agent's enrollment is notice plus `apb passkey list`, not a
  refusal.
- The gate still does not prove a person decided; it proves an
  authenticator holding the enrolled key answered within two minutes.
- The store has no tamper proof, and a same-user writer of
  `<config_dir>` is out of scope, like for every other apb guard: it can
  append its own credential into `passkeys.yaml` (which sends no notice;
  the notice and `apb passkey list` cover only enrollments through apb's
  own surfaces), and it can delete the file, after which every gate
  reads "no passkey enrolled" and the requirement switches off silently;
  the only trace is the `apb doctor` warning row. Nothing inside the
  same uid can be made tamper-proof against such a writer: it can
  replace the `apb` binary, the dashboard assets and the installation
  key too, and a MAC on the store keyed with the installation key would
  be re-signed by an apb-aware writer, since the key sits beside the
  store. What the feature claims is the threat table above: apb's
  surfaces, the run files and the isolation sandbox's credential
  locations, never the config dir against its own user.

## 4. Deliberately out of scope

- Same-user browser driving and virtual authenticators (above).
- Passkeys per project or per playbook; the store is per machine.
- Roaming between machines; the store is local to the config dir.
- Attestation verification; `attestation: none` only.
- Protecting judge decisions, ordinary gates or questions; only
  person-only gates, approvals and publish consent are bound.
