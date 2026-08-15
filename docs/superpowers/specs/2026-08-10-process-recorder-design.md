# Process Recorder: capture-by-observation design

Status: DEFERRED (2026-08-15). The design remains valid and is preserved for a
future stage; no implementation is planned right now. The near-term direction
is the opposite entry point: the user describes the desired playbook in a
guided interview instead of showing it by hand. The companion implementation
plan `docs/superpowers/plans/2026-08-10-process-recorder-mvp.md` is deferred
with this spec. Date of the original approval: 2026-08-10.

## What this is

A tool that records how a person does a routine task once, then turns that
recording into a draft apb playbook. The person presses record, performs the
task the way they normally would, presses stop. The tool produces two
artifacts: a human-readable "show protocol" (the steps in plain language) and,
after the person reviews it and answers a few clarifying questions, a draft
playbook carrying an explicit goal. The tool itself automates nothing; the
existing apb engine runs the resulting playbook. This is the missing bridge
from "showed it by hand" to "have a process", not a re-implementation of what
apb already does.

The framing is deliberate and shapes every decision below: this is consented
apprenticeship under the person's control, not surveillance. Record on demand,
a visible recording indicator, a mandatory review screen, and the right to
delete are consequences of that framing, not add-ons.

## Scope

**MVP is browser-only.** The recorder is a browser extension and nothing else
in the first version. The browser is the one surface where action data is both
cheap and structurally reliable (the page structure and element labels are
readable directly, with none of the "blind app" problem that desktop capture
has), and an extension deploys across a company through standard managed-browser
mechanisms. Desktop capture (macOS first, then Windows) is the next stage and is
described here so the architecture is built for it, but it is not implemented in
the MVP.

**Replay is out of scope, permanently for this tool.** The three layers are
capture (what the person did), reconstruction (raw events folded into meaningful
steps), and running the result. This tool owns capture and reconstruction and
produces a playbook; the existing apb engine runs it. Reliable unattended replay
(a desktop "smart repeat" with an element-identification ladder and state
checks) is a separate, later product.

### Explicitly not in the MVP

- Desktop capture on any OS (browser extension only).
- Replay / execution and any desktop smart-repeat (that is the apb engine and a
  later product).
- Always-on background observation (record-on-demand only).
- Vision/OCR element resolution for surfaces with no structure (rarely needed in
  the browser; belongs to the desktop stage).
- Multi-user collaboration on recordings.
- Automatic PII masking, exclusion lists, and cloud-send redaction detail
  (described under Privacy as the next stage; the MVP ships the basic level
  only).

## Architecture

Three layers, built and tested independently. Two of the three are
platform-independent and are written once; only capture has a
platform/surface-specific implementation.

1. **Capture** (surface-specific). In the MVP this is the browser extension. It
   emits a typed event stream for user actions on a page: clicks, text input,
   select/checkbox/radio changes, key presses, navigation, tab lifecycle. Each
   event carries the page URL, frame URL, a structural locator (xpath + css
   selector), the element tag, and the element's semantic label (its accessible
   name / aria-label / placeholder / nearby text), plus the value read or
   entered and a monotonic timestamp. The extension talks to the core over the
   browser's native-messaging channel when a local core is present, or persists
   locally and hands the session file to the core otherwise (MVP may run fully
   inside the extension plus a thin local core; the transport is an
   implementation detail settled in the plan).

2. **Reconstruction** (platform-independent). A deterministic pass first
   (rule-based, no model): fold consecutive keystrokes into one "typed text"
   event, drop pointer movement that does not end in an action, coalesce
   click down/up into one click, aggregate scrolls, deduplicate. Then a
   segmentation pass by strong boundary signals: navigation (URL change), tab or
   document switch, clipboard write, and commit actions (Enter, or a click on an
   element whose label matches save/submit/send/create/delete). Only then a
   single model pass over the compact, segmented document (text plus a few small
   image crops, never the raw stream) that names each step, describes it in
   plain language, marks which data is read vs entered, flags which values likely
   vary per run, emits a per-step confidence, and marks any point where the
   person chose among visible alternatives as an unexplained choice.

3. **Draft** (platform-independent). Assembles the reviewed protocol into a
   draft apb playbook: an explicit goal, verifiable goal criteria, the steps
   with plain-language labels, the parameters that vary, and the branch points.
   The draft is handed to the normal apb registry; from there the existing
   engine owns it.

**Shared core.** A single event log on one monotonic clock, stored locally
(SQLite for events, a local blob store for image crops). The
platform/surface-specific capture writes into this log through an abstraction so
the desktop stage adds a new capture implementation without touching
reconstruction or draft assembly.

**Review screen.** The primary interaction of the product, not a detail. The
person sees the show protocol, edits step labels, answers clarifying questions,
marks or removes anything private, and confirms goal criteria. Nothing is used
or exported before this screen.

## Data flow

Press record -> extension records the page event stream -> press stop -> core
runs deterministic consolidation (fold typing, drop noise, cut into steps by
navigation / tab / clipboard / commit) -> single model pass names steps and
extracts intent -> **show protocol** (human-readable step list) -> person
reviews, edits, answers clarifying questions, confirms goal criteria -> core
assembles the **draft playbook** with goal and criteria -> playbook enters the
normal apb registry and the existing engine owns it from there.

The model enters late and on compact data. It never sees the raw event stream,
only the consolidated, segmented steps with a small number of crops. This is
both cheaper and the main defense against confident-but-wrong reconstruction.

## The draft playbook

Every playbook produced from a recording carries:

- **Goal**, in explicit words: for example "the invoice is recorded in the
  tracking sheet and sent for approval".
- **Verifiable goal criteria**: how a machine confirms the goal was reached (a
  row with the invoice amount appears in the sheet; the email is in Sent). The
  clarifying interview is where these are elicited: "how do you yourself know you
  did this right?"
- **Steps**, with plain-language labels, marking which data is read, which is
  entered, and which values vary per run (the invoice number is a variable, the
  recipient is fixed).
- **Branch points**: where the person chose among alternatives. The model marks
  these as an explicit "there was a choice here, reason unknown" and asks rather
  than inventing a rule.

**Goal-directed repair, with a hard rule.** The goal is a first-class part of
the playbook. When a run does not reach the goal, the apb supervisor repairs,
adapts, or restructures the process until the goal is reached, by legitimate
means only, no dirty hacks. The hard rule: the supervisor may change the process
however it needs to, but it may not change the goal criteria. Weakening one's own
exam is forbidden to the machine; only a person may change the criteria. This
uses mechanisms the apb engine already has (the supervisor can patch a playbook,
retry a node, continue from a point); what is new is that the goal is a mandatory
part of the playbook and repair is a mandatory response rather than a manual
intervention.

## Goal verification (defense in depth)

Reaching the goal is confirmed in layers, cheapest and most reliable first:

1. **Verifiable criteria (primary).** The goal is expressed as facts the system
   checks directly (row present? email in Sent?), not as the agent's own report
   of success. This is the strongest layer but does not cover everything.
2. **Independent skeptic (for the machine-uncheckable part).** A separate agent
   that did not do the work reviews the result and looks for reasons it is
   wrong. Catches what direct checks cannot; can itself err.
3. **Human as final arbiter (early on).** While trust is not yet earned, a
   person confirms the goal was reached, delivered as an approval: "the process
   believes the goal is met, here is the evidence, confirm?" This check recedes
   along the trust ladder, like every other gate.

## Privacy

**MVP ships the basic level only:**

- Record on demand only. No background observation. A visible "recording"
  indicator. Buttons to delete the last 30 seconds and to delete the whole
  recording.
- Private/incognito browser windows are never recorded.
- Password fields are never recorded.
- Local by default: nothing leaves the machine until the person explicitly
  exports. If reconstruction uses a cloud model, the sending is surfaced in the
  UI at the moment it happens.
- A mandatory review screen before anything is used or exported. Until automatic
  masking exists, this screen is also where the person removes anything private
  by hand, which is why it is mandatory and not optional.

**Described here, next stage, not in the MVP:** automatic masking of sensitive
values at capture time (card numbers, account numbers, phones, addresses,
replaced with labels before storage, locally, no cloud); per-site exclusion
lists (banking, mail, HR systems); and sending only masked step text and
redacted crops to a cloud model with that fact shown in the UI.

## Honesty over guessing

Three rules built in, because the reconstructed output is the whole product and
a plausible-but-wrong step list is worse than an obviously incomplete one:

- **Show capture quality.** Where the extension could not see something (a
  canvas-rendered page, a nonstandard component), the step is marked "limited
  context here" and becomes a question to the person, never silent noise.
- **Halt, do not guess.** A step the model cannot ground is surfaced to the
  person for a five-second clarification rather than filled with a guess.
- **Confidence and branches surfaced.** Every step carries a confidence marker;
  irreversible steps (send, pay, delete) are marked specially. Nothing runs
  automatically from a single unreviewed recording.

## Error handling

- **Capture channel liveness.** The extension reports its own health; if the
  content script is blocked on a page (browser-internal pages, the extension
  store, some enterprise policies) the segment is marked "unattributed" rather
  than appearing as "the person did nothing".
- **Clock alignment.** When a separate local core is present, the extension and
  core exchange a sync ping on connect and re-sync periodically, so events land
  on one timeline; every event is stamped with both raw and corrected time.
- **Empty or trivial recordings.** A recording with no groundable actions
  produces no draft playbook and says so, rather than an empty playbook.
- **Model pass failure.** If the single model pass fails or times out, the show
  protocol falls back to the deterministic step list (named generically) so the
  person still has something to review; reconstruction can be re-run.

## Testing

Three layers, tested separately, no live users required (emulation suffices, per
the strategy decision to defer a live pilot):

- **Consolidation and segmentation (rules).** Plain unit tests over recorded raw
  event streams: given this input stream, expect these steps. Deterministic and
  fast.
- **Model reconstruction.** A set of recorded reference sessions (our synthetic
  invoices, typical emails) each with an expected protocol; compare against it.
- **End-to-end.** A recorded session runs all the way to a draft playbook, and
  that playbook runs under the normal apb engine against an emulated scenario.

## Open questions for the plan

- The exact transport between the extension and the core in the MVP (fully
  in-extension with a thin core, vs native messaging to a local core) is settled
  in the implementation plan; the design allows either.
- The draft playbook's on-disk shape reuses the existing apb playbook schema;
  whether goal and goal-criteria are new first-class schema fields or a
  structured convention within existing fields is a plan-level decision with a
  validator impact.
