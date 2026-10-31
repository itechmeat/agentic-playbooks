//! What a `git push --porcelain` reported for one ref, read into the
//! ledger's three outcomes. Only a ref line git printed for exactly the
//! pushed ref confirms or refuses; everything else (no ref line, a kill, a
//! remote that failed to report status) is unknown, and an unknown push is
//! never repeated on its own.

/// The ledger outcome of one push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The remote ref now points at the pushed commit.
    Confirmed,
    /// The remote refused before updating: nothing was sent.
    NotSent(String),
    /// Whether the remote updated cannot be told.
    Unknown(String),
}

/// Classifies a finished push of `full_ref` (`refs/heads/x`). `killed`:
/// the push was stopped (its timeout, an abort) before it ended by itself.
pub fn classify(stdout: &str, exit: Option<i32>, killed: bool, full_ref: &str) -> Outcome {
    if killed {
        return Outcome::Unknown("the push was stopped before it reported".into());
    }
    let Some((flag, summary)) = ref_line(stdout, full_ref) else {
        return Outcome::Unknown(match exit {
            Some(code) => format!("git exited {code} without reporting the ref"),
            None => "git ended without reporting the ref".into(),
        });
    };
    match flag {
        ' ' | '+' | '*' | '=' => Outcome::Confirmed,
        '!' if summary.starts_with("[rejected]") || summary.starts_with("[remote rejected]") => {
            Outcome::NotSent(summary.to_string())
        }
        _ => Outcome::Unknown(summary.to_string()),
    }
}

/// The flag and summary of the porcelain line for `full_ref`:
/// `<flag>\t<from>:<to>\t<summary>`.
fn ref_line<'a>(stdout: &'a str, full_ref: &str) -> Option<(char, &'a str)> {
    stdout.lines().find_map(|line| {
        let mut parts = line.splitn(3, '\t');
        let flag = parts.next()?;
        let refs = parts.next()?;
        let summary = parts.next().unwrap_or("");
        let mut chars = flag.chars();
        let (Some(f), None) = (chars.next(), chars.next()) else {
            return None;
        };
        let (_, to) = refs.rsplit_once(':')?;
        (to == full_ref).then_some((f, summary.trim()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: &str = "refs/heads/apb/x";

    fn out(flag: &str, summary: &str) -> String {
        format!("To file:///r.git\n{flag}\tabc:{R}\t{summary}\nDone\n")
    }

    #[test]
    fn classification_table() {
        let cases: [(&str, &str, Option<i32>, bool, &str); 11] = [
            (" ", "a..b", Some(0), false, "confirmed"),
            ("+", "a...b (forced update)", Some(0), false, "confirmed"),
            ("*", "[new branch]", Some(0), false, "confirmed"),
            ("=", "[up to date]", Some(0), false, "confirmed"),
            ("!", "[rejected] (stale info)", Some(1), false, "not_sent"),
            (
                "!",
                "[rejected] (non-fast-forward)",
                Some(1),
                false,
                "not_sent",
            ),
            (
                "!",
                "[remote rejected] (pre-receive hook declined)",
                Some(1),
                false,
                "not_sent",
            ),
            (
                "!",
                "[remote failure] (remote failed to report status)",
                Some(1),
                false,
                "unknown",
            ),
            ("!", "[no match]", Some(1), false, "unknown"),
            ("-", "[deleted]", Some(0), false, "unknown"),
            ("*", "[new branch]", Some(0), true, "unknown"),
        ];
        for (flag, summary, exit, killed, want) in cases {
            let got = match classify(&out(flag, summary), exit, killed, R) {
                Outcome::Confirmed => "confirmed",
                Outcome::NotSent(_) => "not_sent",
                Outcome::Unknown(_) => "unknown",
            };
            assert_eq!(got, want, "{flag:?} {summary}");
        }
    }

    #[test]
    fn no_line_for_the_ref_is_unknown() {
        assert!(matches!(
            classify("", Some(128), false, R),
            Outcome::Unknown(_)
        ));
        let other = "To r\n*\tabc:refs/heads/other\t[new branch]\nDone\n";
        assert!(matches!(
            classify(other, Some(0), false, R),
            Outcome::Unknown(_)
        ));
    }
}
