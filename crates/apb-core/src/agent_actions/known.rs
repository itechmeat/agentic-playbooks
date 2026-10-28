//! What the words of a command line are known to mean: the subcommands a
//! program with one is named by, and the options a program or a wrapper
//! (`sudo`, `env`, `time`) takes before what it runs.
//!
//! A word outside these lists never names a step: a subcommand that is not
//! listed is [`OTHER`], and an option that is not listed makes the words
//! after it unreadable (its value may be the next word), so no argument
//! value can reach a step token.

/// What an unknown subcommand or program is named.
pub const OTHER: &str = "other";

/// Where the first word that is not an option stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lead {
    At(usize),
    /// An option this table does not know: what follows cannot be told
    /// from its value.
    Unknown,
    /// Options only.
    End,
}

/// The options a program or a wrapper takes before its first plain word.
pub(super) struct Options {
    /// Options whose value is the next word.
    pub value: &'static [&'static str],
    /// Options that take no value.
    pub flag: &'static [&'static str],
    /// Leading words that are not options and not the subcommand either: a
    /// `+toolchain` (cargo), a `KEY=VALUE` assignment (make).
    pub skip: fn(&str) -> bool,
}

fn none(_: &str) -> bool {
    false
}

fn toolchain(w: &str) -> bool {
    w.starts_with('+')
}

fn assignment(w: &str) -> bool {
    !w.starts_with('-') && w.contains('=')
}

/// No option known: any option before the first plain word is unknown.
pub(super) const NO_OPTIONS: Options = Options {
    value: &[],
    flag: &[],
    skip: none,
};

impl Options {
    /// The first plain word of `words`: options this table knows are
    /// skipped (with their value), `--option=value` too, and `--` ends
    /// the options.
    pub fn lead<S: AsRef<str>>(&self, words: &[S]) -> Lead {
        let mut i = 0;
        while let Some(w) = words.get(i).map(AsRef::as_ref) {
            if w == "--" {
                return if i + 1 < words.len() {
                    Lead::At(i + 1)
                } else {
                    Lead::End
                };
            }
            if (self.skip)(w) {
                i += 1;
            } else if !w.starts_with('-') || w == "-" {
                return Lead::At(i);
            } else if self.value.contains(&w) {
                i += 2;
            } else if self.flag.contains(&w) || (w.starts_with("--") && w.contains('=')) {
                i += 1;
            } else {
                return Lead::Unknown;
            }
        }
        Lead::End
    }
}

/// A program named by its subcommand.
pub(super) struct Program {
    pub name: &'static str,
    pub options: Options,
    pub subs: &'static [&'static str],
    /// Subcommands with a second level, and the words that level takes.
    pub second: &'static [(&'static str, &'static [&'static str])],
}

impl Program {
    pub fn second_level(&self, sub: &str) -> Option<&'static [&'static str]> {
        self.second.iter().find(|(s, _)| *s == sub).map(|(_, l)| *l)
    }
}

/// What gh's command groups take next.
const GH_VERBS: &[&str] = &[
    "add", "archive", "cancel", "checkout", "checks", "clone", "close", "code", "comment",
    "commits", "create", "delete", "develop", "diff", "disable", "download", "edit", "enable",
    "fork", "issues", "list", "lock", "login", "logout", "merge", "prs", "ready", "refresh",
    "remove", "rename", "reopen", "repos", "rerun", "review", "run", "set", "status", "sync",
    "token", "transfer", "unlock", "upload", "view", "watch",
];

/// The JavaScript package managers' commands.
const JS_SUBS: &[&str] = &[
    "add",
    "audit",
    "build",
    "check",
    "ci",
    "create",
    "dev",
    "dlx",
    "exec",
    "format",
    "i",
    "init",
    "install",
    "link",
    "lint",
    "list",
    "ls",
    "outdated",
    "pack",
    "pm",
    "prune",
    "publish",
    "rebuild",
    "remove",
    "rm",
    "run",
    "start",
    "test",
    "uninstall",
    "up",
    "update",
    "upgrade",
    "version",
    "view",
    "why",
    "x",
];

const PROGRAMS: &[Program] = &[
    Program {
        name: "git",
        options: Options {
            value: &[
                "-C",
                "-c",
                "--git-dir",
                "--work-tree",
                "--namespace",
                "--config-env",
                "--exec-path",
            ],
            flag: &[
                "--no-pager",
                "-p",
                "--paginate",
                "-P",
                "--bare",
                "--no-replace-objects",
                "--literal-pathspecs",
                "--glob-pathspecs",
                "--noglob-pathspecs",
                "--icase-pathspecs",
                "--no-optional-locks",
                "--no-advice",
            ],
            skip: none,
        },
        subs: &[
            "add",
            "am",
            "apply",
            "bisect",
            "blame",
            "branch",
            "cat-file",
            "checkout",
            "cherry-pick",
            "clean",
            "clone",
            "commit",
            "config",
            "describe",
            "diff",
            "fetch",
            "format-patch",
            "gc",
            "grep",
            "init",
            "log",
            "ls-files",
            "ls-remote",
            "merge",
            "mv",
            "notes",
            "pull",
            "push",
            "range-diff",
            "rebase",
            "reflog",
            "remote",
            "reset",
            "restore",
            "rev-parse",
            "revert",
            "rm",
            "shortlog",
            "show",
            "sparse-checkout",
            "stash",
            "status",
            "submodule",
            "switch",
            "tag",
            "worktree",
        ],
        second: &[],
    },
    Program {
        name: "gh",
        options: NO_OPTIONS,
        subs: &[
            "alias",
            "api",
            "attestation",
            "auth",
            "browse",
            "cache",
            "codespace",
            "config",
            "extension",
            "gist",
            "gpg-key",
            "issue",
            "label",
            "org",
            "pr",
            "project",
            "release",
            "repo",
            "ruleset",
            "run",
            "search",
            "secret",
            "ssh-key",
            "status",
            "variable",
            "workflow",
        ],
        second: &[
            ("auth", GH_VERBS),
            ("cache", GH_VERBS),
            ("codespace", GH_VERBS),
            ("gist", GH_VERBS),
            ("issue", GH_VERBS),
            ("label", GH_VERBS),
            ("pr", GH_VERBS),
            ("project", GH_VERBS),
            ("release", GH_VERBS),
            ("repo", GH_VERBS),
            ("run", GH_VERBS),
            ("search", GH_VERBS),
            ("secret", GH_VERBS),
            ("variable", GH_VERBS),
            ("workflow", GH_VERBS),
        ],
    },
    Program {
        name: "cargo",
        options: Options {
            value: &["-C", "-Z", "--config", "--color"],
            flag: &[
                "-q",
                "--quiet",
                "-v",
                "-vv",
                "--verbose",
                "--locked",
                "--offline",
                "--frozen",
            ],
            skip: toolchain,
        },
        subs: &[
            "add",
            "audit",
            "bench",
            "build",
            "check",
            "clean",
            "clippy",
            "deny",
            "doc",
            "expand",
            "fetch",
            "fix",
            "fmt",
            "generate-lockfile",
            "init",
            "install",
            "llvm-cov",
            "metadata",
            "new",
            "nextest",
            "package",
            "publish",
            "remove",
            "run",
            "search",
            "sweep",
            "test",
            "tree",
            "uninstall",
            "update",
            "vendor",
        ],
        second: &[],
    },
    Program {
        name: "npm",
        options: Options {
            value: &["--prefix", "-w", "--workspace"],
            flag: &["-g", "--global", "-s", "--silent", "-ws", "--workspaces"],
            skip: none,
        },
        subs: JS_SUBS,
        second: &[],
    },
    Program {
        name: "pnpm",
        options: Options {
            value: &["-C", "--dir", "-F", "--filter"],
            flag: &[
                "-r",
                "--recursive",
                "-w",
                "--workspace-root",
                "-s",
                "--silent",
            ],
            skip: none,
        },
        subs: JS_SUBS,
        second: &[],
    },
    Program {
        name: "bun",
        options: Options {
            value: &["--cwd"],
            flag: &["--bun", "--silent"],
            skip: none,
        },
        subs: JS_SUBS,
        second: &[],
    },
    Program {
        name: "docker",
        options: Options {
            value: &[
                "--context",
                "-c",
                "-H",
                "--host",
                "--config",
                "-l",
                "--log-level",
            ],
            flag: &["-D", "--debug"],
            skip: none,
        },
        subs: &[
            "build",
            "buildx",
            "compose",
            "container",
            "context",
            "cp",
            "exec",
            "image",
            "images",
            "info",
            "inspect",
            "kill",
            "load",
            "login",
            "logout",
            "logs",
            "network",
            "ps",
            "pull",
            "push",
            "restart",
            "rm",
            "rmi",
            "run",
            "save",
            "start",
            "stats",
            "stop",
            "system",
            "tag",
            "top",
            "version",
            "volume",
        ],
        second: &[],
    },
    Program {
        name: "kubectl",
        options: Options {
            value: &[
                "-n",
                "--namespace",
                "--context",
                "--kubeconfig",
                "--cluster",
                "--user",
                "-s",
                "--server",
            ],
            flag: &["-A", "--all-namespaces"],
            skip: none,
        },
        subs: &[
            "annotate",
            "api-resources",
            "apply",
            "auth",
            "cluster-info",
            "config",
            "cordon",
            "cp",
            "create",
            "debug",
            "delete",
            "describe",
            "diff",
            "drain",
            "edit",
            "exec",
            "explain",
            "expose",
            "get",
            "kustomize",
            "label",
            "logs",
            "patch",
            "port-forward",
            "replace",
            "rollout",
            "run",
            "scale",
            "set",
            "taint",
            "top",
            "uncordon",
            "version",
            "wait",
        ],
        second: &[],
    },
    Program {
        name: "make",
        options: Options {
            value: &["-C", "-f", "--directory", "--file", "--makefile"],
            flag: &[
                "-s",
                "--silent",
                "-k",
                "--keep-going",
                "-B",
                "-n",
                "--dry-run",
            ],
            skip: assignment,
        },
        subs: &[
            "all", "bench", "build", "check", "ci", "clean", "coverage", "deploy", "dev", "dist",
            "docs", "down", "fmt", "format", "gen", "generate", "help", "install", "lint",
            "migrate", "release", "run", "serve", "setup", "start", "stop", "test", "up",
        ],
        second: &[],
    },
    Program {
        name: "uv",
        options: Options {
            value: &["--directory", "--project", "-p", "--python", "--color"],
            flag: &[
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "--offline",
                "--no-cache",
                "--frozen",
                "--locked",
            ],
            skip: none,
        },
        subs: &[
            "add", "build", "cache", "export", "init", "lock", "pip", "publish", "python",
            "remove", "run", "self", "sync", "tool", "tree", "venv", "version",
        ],
        second: &[],
    },
    Program {
        name: "apb",
        options: NO_OPTIONS,
        subs: &[
            "adopt",
            "answer",
            "cache",
            "candidate",
            "connector",
            "dashboard",
            "decide",
            "decisions",
            "detect",
            "dev",
            "discover",
            "doctor",
            "eval",
            "export",
            "grade",
            "import",
            "ingest",
            "init",
            "intent",
            "list",
            "make-global",
            "mcp",
            "memory",
            "migrate",
            "note",
            "notify",
            "pause",
            "problems",
            "profile",
            "projects",
            "resume",
            "review",
            "run",
            "runs",
            "self-update",
            "serve",
            "server",
            "stats",
            "stop",
            "subscriptions",
            "suggestions",
            "tasks",
            "trash",
            "trust",
            "validate",
            "wait",
        ],
        second: &[(
            "connector",
            &[
                "approve", "call", "doctor", "env", "init", "install", "list", "show", "test",
            ],
        )],
    },
];

/// The program named `name` by its subcommand, if it is one.
pub(super) fn program(name: &str) -> Option<&'static Program> {
    PROGRAMS.iter().find(|p| p.name == name)
}

/// What only wraps the program it runs, and the options it takes first.
pub(super) fn wrapper(name: &str) -> Option<&'static Options> {
    const SUDO: Options = Options {
        value: &[
            "-u",
            "-g",
            "-C",
            "-D",
            "-h",
            "-p",
            "-U",
            "-r",
            "-t",
            "-T",
            "--user",
            "--group",
            "--chdir",
            "--host",
            "--prompt",
            "--other-user",
            "--role",
            "--type",
            "--close-from",
            "--command-timeout",
        ],
        flag: &[
            "-E",
            "-H",
            "-n",
            "-S",
            "-k",
            "-K",
            "-b",
            "-i",
            "-s",
            "-P",
            "-A",
            "-B",
            "-N",
            "--preserve-env",
            "--login",
            "--shell",
            "--non-interactive",
            "--stdin",
            "--background",
            "--reset-timestamp",
            "--set-home",
            "--askpass",
            "--bell",
            "--preserve-groups",
        ],
        skip: none,
    };
    const ENV: Options = Options {
        value: &["-u", "--unset", "-C", "--chdir"],
        flag: &[
            "-i",
            "--ignore-environment",
            "-0",
            "--null",
            "-v",
            "--debug",
        ],
        skip: none,
    };
    const TIME: Options = Options {
        value: &["-f", "--format", "-o", "--output"],
        flag: &[
            "-p",
            "--portability",
            "-v",
            "--verbose",
            "-a",
            "--append",
            "-q",
            "--quiet",
        ],
        skip: none,
    };
    const COMMAND: Options = Options {
        value: &[],
        flag: &["-p"],
        skip: none,
    };
    const EXEC: Options = Options {
        value: &["-a"],
        flag: &["-c", "-l"],
        skip: none,
    };
    match name {
        "sudo" => Some(&SUDO),
        "env" => Some(&ENV),
        "time" => Some(&TIME),
        "nohup" => Some(&NO_OPTIONS),
        "command" => Some(&COMMAND),
        "exec" => Some(&EXEC),
        _ => None,
    }
}
