---
name: apb-security-audit
description: A focused, read-only security review of one change. Use on every diff the security review role reads, and when a change touches trust boundaries, input handling, files, permissions, secrets or dependencies. Each finding needs a concrete way to exploit it or a concrete failure.
metadata:
  version: "1.1.0"
---

# Audit a change for security

Review the diff and the code it reaches, not the whole repository. The
question for every line: can someone who should not control this, control
it, and what do they gain?

Read-only. Do not run untrusted code from the change outside a sandbox, do
not touch live services, real accounts or shared machines, and use made-up
credentials in any proof.

## 1. Map the boundaries

Before reading for bugs, write down for this change:

- **Sources of untrusted data:** user input, files from the repository or the
  network, environment variables, issue and pull request text, tool and
  connector responses, and the output of any model or agent. Model output is
  untrusted even when the model is your own.
- **Authority:** what can act: shell commands, file writes, network calls,
  credentials, publishing, approvals, an agent's tools.
- **Sinks:** where data turns into action: a command line, a query, a path, a
  template, a header, a log line, a prompt another agent follows.

A finding is a path from a source to authority or a sink without a check that
holds.

## 2. Check each area the diff touches

- **Trust boundaries.** Data crosses from less trusted to more trusted code
  only through validation. A check on the client, in a comment or in a prompt
  is not a check.
- **Untrusted input to authority.** Text from an issue, a file or a model
  never chooses a command, a target, a label, a recipient or a permission by
  itself. Agent tool use is bounded by what the step needs.
- **Paths and links.** Joined paths are checked after normalization; `..`,
  absolute paths and symbolic links cannot escape the intended root; a check
  and the use that follows it see the same file.
- **Privilege and file modes.** New files with secrets or state are private.
  Nothing runs with more rights than it needs. Defaults are the safe choice.
- **Secrets.** No secret in a log, an error, a report, a URL, a command line
  argument or a cache. Comparisons of secret values take constant time.
- **Injection.** Shell, query, template, header, markup and prompt
  injection: values are passed as data, never spliced into code.
- **Races on writes.** Writes are atomic where a reader may see them; locks
  cover the whole check-then-act; a crash leaves a recoverable state.
- **Consent and authority bypass.** Every route to a privileged action goes
  through the same gate: retries, resumes, alternate entry points and
  defaults included. Nothing turns a refusal into a silent success.
- **Supply chain.** New dependencies, version ranges, install scripts, build
  steps that fetch code, unpinned actions in automation.

## 3. Prove each finding

A finding states:

- a location: file and line or range in the diff;
- the attacker or the failure: who controls which input;
- the path: the steps from that input to the harm, in order;
- the impact: what is read, changed, run or lost;
- a severity: `critical` (remote or unattended compromise, secret
  disclosure at scale), `high` (privilege gained or a guard bypassed with a
  realistic precondition), `medium` (needs an unusual precondition or gives
  limited reach), `low` (hardening with a plausible path);
- a fix: the smallest change that breaks the path.

Without a concrete path it is not a finding. Record it as a question or a
hardening note instead, labelled as such. Verify every finding against the
source; say what you could not verify.

## 3a. Clean up

A proof may need scratch inputs, a local server, a browser session or a test
account. Create scratch files under one directory named for the audit, and
before you report, or when the audit stops early on a failure, delete them, stop the servers and close the sessions you
started, and remove a test account you created where you are allowed to.
Delete only what the audit created. The finding keeps its evidence as text.

## 4. Report

- List findings by severity, then the areas you checked with no finding,
  then what you did not cover.
- In a public repository, never put an exploitable finding in a public issue,
  comment, commit message or pull request. Report it privately to the
  maintainers through the project's security policy, and keep only a neutral
  pointer in public.
- Missing coverage is reported as missing, never as clean.
