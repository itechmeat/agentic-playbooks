//! The apb binaries that drove a run, from its journal: the start's
//! `run_provenance.apb` and every `run_resumed.apb`.
//!
//! Display only. An apb older than the field writes neither, so a drive
//! without a version is "an older apb", never a guess at which one. A
//! downgrade is a drive by an apb older than an earlier one: a lower
//! version, or no version after a recorded one.

use std::cmp::Ordering;

use crate::event::{Event, EventPayload};

/// One drive of a run: its start or a resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// The seq of its `run_provenance` or `run_resumed`.
    pub seq: u64,
    pub resumed: bool,
    /// The binary version it recorded; `None` for an apb that records none.
    pub apb: Option<String>,
}

/// The longest version string a surface shows.
const VERSION_MAX: usize = 64;

/// A recorded version as the surfaces show it: only the characters a
/// version is made of (letters, digits, `.`, `-`, `+`), at most
/// [`VERSION_MAX`]. The journal is a plain file an agent step without
/// isolation can write, so its text never reaches a terminal as written.
pub fn clean_version(v: &str) -> String {
    v.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        .take(VERSION_MAX)
        .collect()
}

/// Every drive of a run, in journal order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunBinaries {
    pub drives: Vec<Drive>,
}

/// A drive by an apb older than an earlier drive's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downgrade {
    pub seq: u64,
    /// The newest version that drove the run before it.
    pub from: String,
    pub to: Option<String>,
}

impl RunBinaries {
    pub fn from_events(events: &[Event]) -> Self {
        let drives = events
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::RunProvenance { apb, .. } => Some(Drive {
                    seq: e.seq,
                    resumed: false,
                    apb: apb.as_deref().map(clean_version),
                }),
                EventPayload::RunResumed { apb, .. } => Some(Drive {
                    seq: e.seq,
                    resumed: true,
                    apb: apb.as_deref().map(clean_version),
                }),
                _ => None,
            })
            .collect();
        Self { drives }
    }

    /// Whether any drive recorded its version: a run only older binaries
    /// drove has nothing to say.
    pub fn any_recorded(&self) -> bool {
        self.drives.iter().any(|d| d.apb.is_some())
    }

    /// "driven by apb X, resumed by apb Y, an older apb", consecutive
    /// equal resumers named once with a count; `None` when no drive
    /// recorded a version.
    pub fn line(&self) -> Option<String> {
        if !self.any_recorded() {
            return None;
        }
        let started = self
            .drives
            .iter()
            .find(|d| !d.resumed)
            .map_or_else(|| "an unknown apb".to_string(), |d| label(d.apb.as_deref()));
        let mut resumers: Vec<(String, usize)> = Vec::new();
        for d in self.drives.iter().filter(|d| d.resumed) {
            let name = label(d.apb.as_deref());
            match resumers.last_mut() {
                Some((last, n)) if *last == name => *n += 1,
                _ => resumers.push((name, 1)),
            }
        }
        let mut out = format!("driven by {started}");
        if !resumers.is_empty() {
            let list: Vec<String> = resumers
                .into_iter()
                .map(|(name, n)| match n {
                    1 => name,
                    n => format!("{name} ({n} times)"),
                })
                .collect();
            out.push_str(&format!(", resumed by {}", list.join(", ")));
        }
        Some(out)
    }

    /// Every drive by an apb older than one before it.
    pub fn downgrades(&self) -> Vec<Downgrade> {
        let mut newest: Option<&str> = None;
        let mut out = Vec::new();
        for d in &self.drives {
            match (newest, d.apb.as_deref()) {
                (Some(prev), None) => out.push(Downgrade {
                    seq: d.seq,
                    from: prev.to_string(),
                    to: None,
                }),
                (Some(prev), Some(v)) if compare(v, prev) == Ordering::Less => {
                    out.push(Downgrade {
                        seq: d.seq,
                        from: prev.to_string(),
                        to: Some(v.to_string()),
                    })
                }
                (Some(prev), Some(v)) if compare(v, prev) == Ordering::Greater => newest = Some(v),
                (None, Some(v)) => newest = Some(v),
                _ => {}
            }
        }
        out
    }
}

/// How a drive's binary reads in a line.
pub fn label(apb: Option<&str>) -> String {
    match apb {
        Some(v) => format!("apb {v}"),
        None => "an older apb (no version recorded)".to_string(),
    }
}

/// Orders two apb versions (`1.2.3`, `1.0.0-beta.2`) by semantic version
/// precedence: numbers first, a pre-release before its release, pre-release
/// identifiers compared numerically when both are numbers.
pub fn compare(a: &str, b: &str) -> Ordering {
    let split = |v: &str| -> (Vec<u64>, Option<String>) {
        let v = v.split('+').next().unwrap_or(v);
        let (core, pre) = match v.split_once('-') {
            Some((c, p)) => (c, Some(p.to_string())),
            None => (v, None),
        };
        (
            core.split('.').map(|n| n.parse().unwrap_or(0)).collect(),
            pre,
        )
    };
    let ((ca, pa), (cb, pb)) = (split(a), split(b));
    let seg = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    let core = (0..ca.len().max(cb.len()))
        .map(|i| seg(&ca, i).cmp(&seg(&cb, i)))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal);
    if core.is_ne() {
        return core;
    }
    match (pa, pb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let (xs, ys): (Vec<&str>, Vec<&str>) = (x.split('.').collect(), y.split('.').collect());
            for (p, q) in xs.iter().zip(&ys) {
                let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    _ => p.cmp(q),
                };
                if o.is_ne() {
                    return o;
                }
            }
            xs.len().cmp(&ys.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drives(list: &[(bool, Option<&str>)]) -> RunBinaries {
        RunBinaries {
            drives: list
                .iter()
                .enumerate()
                .map(|(i, (resumed, apb))| Drive {
                    seq: i as u64 * 10,
                    resumed: *resumed,
                    apb: apb.map(str::to_string),
                })
                .collect(),
        }
    }

    #[test]
    fn versions_order_by_semver_precedence() {
        assert_eq!(compare("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare("1.0.0-beta.2", "1.0.0-beta.10"), Ordering::Less);
        assert_eq!(compare("1.0.0-beta.2", "1.0.0"), Ordering::Less);
        assert_eq!(compare("0.25.0", "1.0.0-beta.1"), Ordering::Less);
        assert_eq!(compare("1.1.0", "1.0.9"), Ordering::Greater);
        assert_eq!(compare("1.0.0-alpha", "1.0.0-alpha.1"), Ordering::Less);
    }

    #[test]
    fn the_line_names_the_start_and_each_resumer() {
        assert_eq!(drives(&[(false, None), (true, None)]).line(), None);
        let b = drives(&[
            (false, Some("1.1.0")),
            (true, Some("1.1.0")),
            (true, Some("1.1.0")),
            (true, None),
        ]);
        assert_eq!(
            b.line().unwrap(),
            "driven by apb 1.1.0, resumed by apb 1.1.0 (2 times), an older apb (no version recorded)"
        );
        assert_eq!(
            drives(&[(false, None), (true, Some("1.1.0"))])
                .line()
                .unwrap(),
            "driven by an older apb (no version recorded), resumed by apb 1.1.0"
        );
    }

    #[test]
    fn a_downgrade_is_a_lower_or_unrecorded_version_after_a_recorded_one() {
        assert!(
            drives(&[(false, None), (true, Some("1.1.0"))])
                .downgrades()
                .is_empty()
        );
        let d = drives(&[
            (false, Some("1.2.0")),
            (true, Some("1.1.0")),
            (true, Some("1.3.0")),
            (true, None),
        ])
        .downgrades();
        assert_eq!(
            d,
            vec![
                Downgrade {
                    seq: 10,
                    from: "1.2.0".into(),
                    to: Some("1.1.0".into())
                },
                Downgrade {
                    seq: 30,
                    from: "1.3.0".into(),
                    to: None
                },
            ]
        );
    }
}
