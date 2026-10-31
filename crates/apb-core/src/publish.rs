//! The publish step's pure parts: the ledger identity of a push, the plan's
//! refusal codes, the plan digest and the nonce a person's consent carries,
//! and the parsers of what the push touches ([`url`], [`refname`]) and what
//! it reports ([`porcelain`]). The engine runs the step
//! (`apb_engine::publish`); the one git call allowed a transport is
//! [`crate::git::transport`].

pub mod porcelain;
pub mod refname;
pub mod url;

use serde::{Deserialize, Serialize};

/// The engine site a push is recorded under in the send ledger. Connector
/// names are safe segments, so an `@` name cannot collide with one.
pub const GIT_CONNECTOR: &str = "@git";
/// The ledger function of a push.
pub const PUSH_FUNCTION: &str = "push";
/// The decision that consents to the plan.
pub const OPTION_PUBLISH: &str = "publish";
/// The decision that declines it.
pub const OPTION_SKIP: &str = "skip";
/// The options a publish gate offers, in order.
pub const OPTIONS: [&str; 2] = [OPTION_PUBLISH, OPTION_SKIP];
/// The push's bound when the node sets none.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
/// The largest `timeout_seconds` a node may set.
pub const MAX_TIMEOUT_SECONDS: u64 = 3600;
/// The bound of a read-only remote listing (the plan, a reconcile).
pub const LIST_TIMEOUT_SECONDS: u64 = 60;
/// The nonce prefix of a publish consent.
pub const NONCE_PREFIX: &str = "publish";

/// The node outputs a publish node finishes with (the text before any
/// detail).
pub mod output {
    pub const PUBLISHED: &str = "published";
    pub const SKIPPED: &str = "skipped";
    pub const NOTHING_TO_PUBLISH: &str = "nothing_to_publish";
    pub const ALREADY_ON_REMOTE: &str = "already_on_remote";
    pub const REFUSED: &str = "publish_refused";
    pub const NOT_SENT: &str = "publish_not_sent";
    pub const UNKNOWN: &str = "publish_unknown";
}

/// Declares [`Refusal`] with its code and sentence per variant, so
/// [`Refusal::ALL`] can never miss one.
macro_rules! refusals {
    ($($(#[$doc:meta])* $variant:ident => $code:literal, $sentence:literal;)*) => {
        /// Why a plan cannot be offered. Each refusal fails the node and
        /// records nothing in the ledger.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Refusal {
            $($(#[$doc])* $variant,)*
        }

        impl Refusal {
            /// Every refusal, in a stable order.
            pub const ALL: &'static [Refusal] = &[$(Refusal::$variant,)*];

            /// The machine code.
            pub fn code(self) -> &'static str {
                match self {
                    $(Refusal::$variant => $code,)*
                }
            }

            /// One English sentence a person reads.
            pub fn sentence(self) -> &'static str {
                match self {
                    $(Refusal::$variant => $sentence,)*
                }
            }
        }
    };
}

refusals! {
    NoPin => "no_pin",
        "The remote was not pinned before agents ran, so the target cannot be trusted.";
    NotARepository => "not_a_repository",
        "The working tree is not a git repository apb runs git in.";
    RemoteMissing => "remote_missing", "The repository has no such remote.";
    UrlRefused => "url_refused",
        "The remote URL is refused: only https, ssh and file URLs without credentials are pushed to.";
    RemoteChanged => "remote_changed", "The remote URL changed after it was pinned.";
    RepositoryChanged => "repository_changed",
        "The working tree belongs to another repository than the one pinned.";
    HeadMoved => "head_moved", "HEAD is not the commit the source node recorded.";
    BranchMoved => "branch_moved",
        "HEAD is no longer on the branch the source node committed on.";
    BranchInvalid => "branch_invalid", "The branch is not a valid branch name.";
    UntrackedCommits => "untracked_commits",
        "HEAD has commits no node of this run recorded; publish from a node that commits.";
    NotFastForward => "not_fast_forward",
        "The remote branch has commits this tree does not have; integrate them first.";
    RemoteUnreachable => "remote_unreachable", "The remote could not be listed.";
    UrlRewritten => "url_rewritten",
        "The git configuration rewrites the remote URL to another address.";
    RemoteWritable => "remote_writable",
        "The file remote lies inside the working tree or a path a node may write, so its hooks are not the operator's.";
    HistoryUnreadable => "history_unreadable",
        "The commits the push would carry could not be read from the tree.";
    /// A code journaled by another apb version; the detail names it.
    Unrecognized => "unrecognized", "The run recorded a refusal this apb does not know.";
}

impl Refusal {
    /// The refusal a code names.
    pub fn parse(code: &str) -> Option<Refusal> {
        Refusal::ALL.iter().copied().find(|r| r.code() == code)
    }
}

/// What a plan binds: the person consents to exactly these facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFacts<'a> {
    pub node: &'a str,
    pub url: &'a str,
    /// The remote branch, short form (`apb/x`).
    pub branch: &'a str,
    pub after: &'a str,
    pub remote_before: Option<&'a str>,
    /// Every commit the push carries, newest first.
    pub commits: &'a [String],
}

/// The plan digest: a sha256 over every fact the person saw.
pub fn plan_digest(f: &PlanFacts<'_>) -> String {
    let mut input = format!(
        "publish-plan-v1\nnode={}\nurl={}\nbranch={}\nafter={}\nremote_before={}\n",
        f.node,
        f.url,
        f.branch,
        f.after,
        f.remote_before.unwrap_or("")
    );
    for c in f.commits {
        input.push_str("commit=");
        input.push_str(c);
        input.push('\n');
    }
    crate::content::sha256_hex(input.as_bytes())
}

/// The nonce a consent to the plan with `digest` carries: `publish-` and 32
/// hex digits. Not a secret; it binds a decision to the plan it answers.
pub fn nonce(digest: &str) -> String {
    crate::content::short_nonce(NONCE_PREFIX, &[digest])
}

/// The ledger label of a push to `branch`.
pub fn ledger_label(branch: &str) -> String {
    format!("ref:{branch}")
}

/// The ledger's effect reference of a push: `<url> refs/heads/<b>
/// <old>..<after>`, `old` empty for a new branch.
pub fn effect_ref(url: &str, branch: &str, remote_before: Option<&str>, after: &str) -> String {
    format!(
        "{url} {} {}..{after}",
        refname::full(branch),
        remote_before.unwrap_or("")
    )
}

/// The first 12 characters of a commit id for display; the whole text when
/// it is shorter or not plain ASCII there (a hand-edited journal never
/// panics a view).
pub fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// `value` as one POSIX shell word for a command a person copies (a run id,
/// a node id): quoted when it holds anything but safe characters, so a
/// space or a `;` never splits the command or runs another.
pub fn shell_word(value: &str) -> std::borrow::Cow<'_, str> {
    shell_words::quote(value)
}

/// Whether `decision` is one a publish gate takes.
pub fn is_option(decision: &str) -> bool {
    OPTIONS.contains(&decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(commits: &'a [String]) -> PlanFacts<'a> {
        PlanFacts {
            node: "publish",
            url: "https://example.com/r.git",
            branch: "apb/x",
            after: "a1",
            remote_before: None,
            commits,
        }
    }

    #[test]
    fn the_digest_is_stable_and_every_fact_changes_the_nonce() {
        let commits = vec!["a1".to_string()];
        let base = facts(&commits);
        let d = plan_digest(&base);
        assert_eq!(d, plan_digest(&base.clone()));
        let n = nonce(&d);
        assert!(n.starts_with("publish-") && n.len() == "publish-".len() + 32);
        let more = vec!["a1".to_string(), "b2".to_string()];
        let variants = [
            PlanFacts {
                node: "other",
                ..base.clone()
            },
            PlanFacts {
                url: "ssh://h/r.git",
                ..base.clone()
            },
            PlanFacts {
                branch: "apb/y",
                ..base.clone()
            },
            PlanFacts {
                after: "a2",
                ..base.clone()
            },
            PlanFacts {
                remote_before: Some("c3"),
                ..base.clone()
            },
            PlanFacts {
                commits: &more,
                ..base.clone()
            },
        ];
        for v in variants {
            assert_ne!(nonce(&plan_digest(&v)), n, "{v:?}");
        }
    }

    #[test]
    fn refusal_codes_round_trip() {
        for &r in Refusal::ALL {
            assert_eq!(Refusal::parse(r.code()), Some(r));
            assert!(r.sentence().ends_with('.'));
        }
        assert_eq!(Refusal::parse("nope"), None);
    }

    #[test]
    fn a_copied_value_is_one_shell_word() {
        assert_eq!(shell_word("ship"), "ship");
        assert_eq!(shell_word("ship release"), "'ship release'");
        assert_eq!(shell_word("a;rm x"), "'a;rm x'");
    }

    #[test]
    fn a_short_sha_never_splits_a_character() {
        assert_eq!(short_sha("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha("01234567890\u{e9}x"), "01234567890\u{e9}x");
    }

    #[test]
    fn the_effect_ref_names_the_full_ref_and_the_range() {
        assert_eq!(
            effect_ref("file:///r.git", "apb/x", None, "ab"),
            "file:///r.git refs/heads/apb/x ..ab"
        );
        assert_eq!(
            effect_ref("file:///r.git", "main", Some("cd"), "ab"),
            "file:///r.git refs/heads/main cd..ab"
        );
    }
}
