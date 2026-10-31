---
decided: 2026-10-28
decision-makers: apb maintainers
---

# apb's own git commands never run a program the working tree's configuration, attributes or hooks name

## Applies to

Every git command apb itself runs on a working tree: run provenance (`HEAD`
and the commits a node made), the tree lock's `git rev-parse`, the cache and
baseline fingerprints, the repository fingerprint of a workspace, the
known-problem source reader, the trial's worktree, status and diff, the
sandbox's repository facts and the eval runner
(`crates/apb-core/src/git.rs`).

What is guaranteed, for every git apb runs on a repository through
`apb_core::git::Repo` (and `read`, `succeeds` and `command`, which use it):

- Every `GIT_*` variable of apb's environment is removed, and no pager runs
  (`--no-pager`).
- With `-c`, which outranks every config file and every file one includes:
  `core.fsmonitor=false`, `core.hooksPath=/dev/null` (no hook can exist
  there), `log.showSignature=false` (no `gpg.program`),
  `protocol.allow=never` (no transport, so no `core.sshCommand`,
  `core.gitProxy`, remote helper, `ext::` command,
  `remote.<name>.uploadpack` or `receivepack`), `credential.helper=` (the
  helper list emptied), `core.askPass=`, and `diff.ignoreSubmodules=dirty`,
  `status.submoduleSummary=false` and `submodule.recurse=false`: no git
  starts inside a submodule for apb, where the submodule's own config,
  hooks and attributes would apply.
- The repository is pinned: `GIT_DIR`, `GIT_COMMON_DIR` and `GIT_WORK_TREE`
  are set to the canonical directories git discovered, and a discovery
  whose common directory is neither the git directory itself nor, for a
  linked worktree, the repository its `worktrees/<name>` belongs to is
  refused (`RepoError::Redirected`). A planted `commondir` therefore never
  hands apb another config: apb runs no git there at all.
- Every filter driver the repository's own config defines (its local and
  worktree scopes, included files counted) has its `clean`, `smudge` and
  `process` emptied and `required` turned off, so a `.gitattributes` line
  naming it runs nothing on a checkout (`worktree add`), a status or a diff.
  Drivers only the operator's global or system config defines (git-lfs)
  keep working.
- Every diff apb takes carries `--no-ext-diff --no-textconv
  --ignore-submodules=dirty`, and every status `--ignore-submodules=dirty`:
  neither `diff.external`, a diff driver's `command` nor its `textconv`
  runs, and no git starts inside a submodule to look at its work tree. A
  change of the commit a submodule points at still shows (git reads the
  submodule's `HEAD` in its own process), so the cache fingerprint and the
  trial diff see a submodule bump. The test checks with `GIT_TRACE` that
  apb's diff and status start no other git at all.
- `GIT_LFS_SKIP_SMUDGE=1`: the operator's own git-lfs leaves pointer files
  on apb's checkouts, so a tree's `.lfsconfig` never makes it download from
  a host the tree names (with the operator's credentials).
- git older than 2.31 is refused (`RepoError::GitTooOld`), and a repository
  apb refuses (redirected, an unreadable config, a git too old) is an error
  to every caller, never a quiet "not a repository": `succeeds` returns it,
  and a caller for whom `false` is the permissive answer fails closed (the
  known-problem source reader then treats every file as git-ignored).

The one exception is the publish step's transport builder,
`apb_core::git::transport` (`crates/apb-core/src/git/transport.rs`): its
push and its remote listings keep every setting above except the emptied
credential helper, so the operator's own helper from their global or system
config works, and allow exactly one transport, the push URL's own scheme.
They run from a shadow git directory made fresh for each call
(`runs/<id>/publish/shadow-<random>`, an empty bare repository reaching the
tree's objects through its alternates file), so the tree's local config,
hooks, `url.*.insteadOf` and `url.*.pushInsteadOf` never apply to them.
Decision record 0010 covers that step.

A caller may add stricter settings after these (the eval runner points hooks
at its own empty directory, reads no system config and allows only the
`file` transport). The eval runner's git after an agent reads blobs only
(`cat-file`), which runs no filter.

In an isolation sandbox the repository is protected for the operator's own
git too (`crates/apb-engine/src/sandbox/git_guard.rs`):

- every git directory's `config`, `hooks/`, `info/` and `config.worktree`
  is read-only (the common one, each linked worktree's, each submodule's
  under `modules/` or embedded in the tree), and so are the `.git` files of
  the tree and of its submodules;
- what redirects git without a mount point to protect it is recorded
  before the attempt and put back after it, which fails the attempt and
  names it: a `commondir`, `gitdir`, new `config.worktree` or
  `objects/info/alternates` in any git directory; a new linked worktree
  that does not lead back to the repository or carries its own config
  (removed whole, with any `config` or `hooks` planted in it); a new
  submodule git directory under `modules/` (removed whole); a new gitlink in
  the index, or a `.git` that appeared for an existing one (its `.git`
  removed, so no git ever enters it);
- nothing is written through a directory the attempt replaced by a link;
  that fails the node at once.

The git directory itself stays writable: a commit renames lock files into
it.

Sibling features, and why they are closed:

- `core.worktree` in a nested repository or a submodule: apb never runs git
  there, and its own `GIT_WORK_TREE` outranks `core.worktree`.
- `include.path` and `includeIf`: an included file can set any key, but
  every key above is passed with `-c` or the environment, which outrank it.
- `uploadpack.packObjectsHook`: git reads it from protected config only
  (system, global, command line), and apb starts no transport.
- `core.hooksPath` or `core.fsmonitor` in a submodule: apb's git never
  enters a submodule; in a sandbox the submodule's config is read-only, and
  a new one loses its `.git`.
- A filter of the operator's own global or system config (git-lfs, a
  `process` filter) still runs on a file the tree's `.gitattributes`
  assigns to it: the operator trusts that program, and git-lfs downloads
  nothing for apb (`GIT_LFS_SKIP_SMUDGE`).

Not covered: git run by a playbook's own scripts and agents (theirs, not
apb's); merge drivers (apb never merges); a repository nested in the tree
without a gitlink (the agent's own, like the rest of the tree: git never
enters it on the operator's behalf); a submodule's own submodules in the
sandbox guard (it records the top level's gitlinks); an included config
file that is a FIFO (git blocks reading it, nothing runs).

## Why

A tree's `.git/config`, `.gitattributes` and hooks are written by whoever
controls the tree: an agent during a run, or the repository someone cloned.
A filesystem monitor, a hooks directory, a signature program, an external
diff, a textconv or a clean/smudge filter named there would run as the
operator, outside any sandbox, the next time apb reads the tree. A
`commondir` file is the lever that makes the rest reachable from a
sandbox, whose config is read-only. The operator's shell may also carry
`GIT_DIR` or `GIT_CONFIG_PARAMETERS` that point apb at another repository or
turn one of these back on.

## Checked by

- apb-engine `git_hardening_test::apb_git_never_runs_a_program_the_tree_names`
  (every vector armed, across a run, the fingerprints with a dirty tracked
  file, and the trial's checkout, status and diff; controls show each trap
  is live for a plain git)
- apb-engine `git_hardening_test::a_planted_commondir_never_hands_apb_another_config`
- apb-engine `git_hardening_test::a_filter_of_the_operators_global_config_still_applies`
- apb-engine `git_hardening_test::a_planted_submodule_never_runs_its_own_filter`
- apb-engine `git_hardening_test::a_submodule_bump_changes_the_fingerprint`
- apb-mcp `trial_tools_test::a_trial_never_runs_a_program_the_tree_names`
- apb-engine `sandbox_run_test::a_planted_git_redirection_is_put_back_and_fails_the_attempt`
- apb-engine `sandbox_run_test::a_planted_submodule_loses_its_git_directory_and_fails_the_attempt`
- apb-engine `known_problems::source::tests::a_private_ignored_or_large_file_is_never_a_source`
  (a redirected repository fails closed)
- apb-engine `sandbox::git_guard::tests` (unit tests)
- apb-core `git::tests` (unit tests)
- apb-engine `eval::git::tests::the_hardened_command_ignores_the_trees_hooks_and_fsmonitor`
  (unit test)
