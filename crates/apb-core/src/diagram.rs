//! The derived diagram of a playbook version (`playbook.mmd`,
//! people-in-the-loop D6): a flowchart text written beside every
//! `playbook.yaml`, never read back. The file is display only: it sits
//! outside every digest and bundle by construction, and its first line
//! carries the YAML's [`crate::scope::digest_str`], so `state_of` can tell a
//! diagram that belongs to its version (`Current`) from one that was never
//! written (`Missing`), one an older render left behind (`Stale`), and one a
//! person or tool edited (`Edited`). Neither a person nor an agent
//! maintains the file: every write and repair apb makes (each version
//! write, each move of `current`, `apb diagram --rebuild`) gives it the
//! diagram of exactly the YAML bytes, so an edit is overwritten, and
//! `Edited` exists only for `apb doctor` between writes. A version
//! directory that cannot be read at all is `Unreadable`, and every scan
//! skips it.

use std::fs;
use std::path::Path;

use crate::schema::{Edge, EdgeCondition, NodeKind, Outcome, Playbook};

/// The file a version directory holds beside `playbook.yaml`.
pub const DIAGRAM_FILE: &str = "playbook.mmd";
/// The first line's prefix; the YAML's digest follows it.
pub const DIGEST_PREFIX: &str = "%% apb-diagram ";
/// The longest label, in characters, after escaping.
pub const LABEL_MAX: usize = 60;

/// The flowchart text (Mermaid syntax) of `playbook`, whose YAML has
/// `digest` ([`crate::scope::digest_str`]). Pure: the same definition gives
/// the same bytes.
pub fn mermaid(playbook: &Playbook, digest: &str) -> String {
    mermaid_with(playbook, digest, &|label| label.to_string())
}

/// [`mermaid`] with every label's raw text passed through `redact` before
/// it is escaped: a reader that scrubs registered values (the run report)
/// matches them in the spelling the author wrote, never in Mermaid's
/// escaped one.
pub fn mermaid_with(playbook: &Playbook, digest: &str, redact: &dyn Fn(&str) -> String) -> String {
    let mut out = String::new();
    out.push_str(DIGEST_PREFIX);
    out.push_str(digest);
    out.push_str(" derived from playbook.yaml; apb diagram --rebuild writes it again\n");
    out.push_str("flowchart TD\n");
    for (i, node) in playbook.nodes.iter().enumerate() {
        let label = cut(&escape(&redact(&node_label(node))));
        let (open, close) = shape(&node.kind);
        out.push_str(&format!("  n{i}{open}\"{label}\"{close}\n"));
    }
    // Declaration order: the addressed edges are aligned with `pb.edges`.
    for addressed in crate::edge_address::edge_addresses(playbook) {
        let edge = &playbook.edges[addressed.index];
        let (Some(a), Some(b)) = (
            node_index(playbook, &edge.from),
            node_index(playbook, &edge.to),
        ) else {
            // An edge naming a node the definition lacks is left out.
            continue;
        };
        match edge_label(edge) {
            Some(label) => {
                let label = cut(&escape(&redact(&label)));
                out.push_str(&format!("  n{a} -->|\"{label}\"| n{b}\n"));
            }
            None => out.push_str(&format!("  n{a} --> n{b}\n")),
        }
    }
    out
}

/// The index of the node with id `id`, or `None` when the definition lacks
/// it.
fn node_index(playbook: &Playbook, id: &str) -> Option<usize> {
    playbook.nodes.iter().position(|n| n.id == id)
}

/// A node's label: `<title or id> (<kind>)`, with a finish's outcome after
/// its kind, a list node's ` list` and a person-only node's
/// ` person only`.
fn node_label(node: &crate::schema::Node) -> String {
    let base = match node.title.as_deref() {
        Some(title) if !title.is_empty() => title,
        _ => node.id.as_str(),
    };
    let kind = match &node.kind {
        NodeKind::Finish { outcome, .. } => format!("finish {}", outcome_name(*outcome)),
        other => other.type_str().to_string(),
    };
    let mut label = format!("{base} ({kind})");
    if node.list.is_some() {
        label.push_str(" list");
    }
    if node.person_only {
        label.push_str(" person only");
    }
    label
}

fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Success => "success",
        Outcome::Failure => "failure",
    }
}

/// The wrappers around a quoted label: `start` and `finish` are a stadium,
/// the four gates a rhombus, everything else a rectangle.
fn shape(kind: &NodeKind) -> (&'static str, &'static str) {
    match kind {
        NodeKind::Start | NodeKind::Finish { .. } => ("([", "])"),
        NodeKind::HumanReview { .. }
        | NodeKind::Publish { .. }
        | NodeKind::Condition { .. }
        | NodeKind::Judge { .. } => ("{", "}"),
        _ => ("[", "]"),
    }
}

/// The label of an edge that carries a condition, is a `fallback`, or
/// has a `join` or a `max_traversals`: its parts joined by `, `; `None`
/// for a plain edge.
fn edge_label(edge: &Edge) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(condition) = &edge.condition {
        parts.push(condition_label(condition));
    }
    if edge.fallback {
        parts.push("fallback".to_string());
    }
    if let Some(join) = &edge.join {
        parts.push(format!("join {join}"));
    }
    if let Some(max) = edge.max_traversals {
        parts.push(format!("max {max}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn condition_label(condition: &EdgeCondition) -> String {
    match condition {
        EdgeCondition::ReviewStatus { equals } => equals.clone(),
        EdgeCondition::OutputField {
            field,
            equals,
            lt,
            lte,
            gt,
            gte,
            ..
        } => {
            // The validator (V169) admits one comparator; read the first
            // that is set.
            let (op, value) = if let Some(v) = equals {
                ("=", v.clone())
            } else if let Some(v) = lt {
                ("<", threshold(v.0))
            } else if let Some(v) = lte {
                ("<=", threshold(v.0))
            } else if let Some(v) = gt {
                (">", threshold(v.0))
            } else if let Some(v) = gte {
                (">=", threshold(v.0))
            } else {
                return field.clone();
            };
            format!("{field} {op} {value}")
        }
        EdgeCondition::OutputMatch { pattern, .. } => format!("matches {pattern}"),
        EdgeCondition::NodeStatus { node, equals } => {
            let status = match equals {
                crate::schema::StatusEq::Success => "success",
                crate::schema::StatusEq::Failure => "failure",
                crate::schema::StatusEq::Stalled => "stalled",
            };
            format!("{node} {status}")
        }
        EdgeCondition::Judge { question, .. } => format!("judge: {question}"),
        EdgeCondition::NoNewItems { field, .. } => format!("no new {field}"),
    }
}

/// A threshold in the text the YAML wrote: `80`, not `80` with a float's
/// trailing baggage. Rust's `Display` for `f64` already gives the shortest
/// form (`80`, `0.8`).
fn threshold(value: f64) -> String {
    format!("{value}")
}

/// Escapes a label for a quoted Mermaid string: every control character and
/// line break becomes a space, then the characters that would close the
/// quote, form a directive or end a label early become entities.
fn escape(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for c in label.chars() {
        if c.is_control() {
            out.push(' ');
        } else {
            match c {
                '#' => out.push_str("#35;"),
                '"' => out.push_str("#quot;"),
                '&' => out.push_str("#amp;"),
                '<' => out.push_str("#lt;"),
                '>' => out.push_str("#gt;"),
                '|' => out.push_str("#124;"),
                '`' => out.push_str("#96;"),
                // Mermaid scans the whole source for `%%{ ... }%%`
                // directives before it parses a quoted label: a `%` in a
                // label is never kept as itself.
                '%' => out.push_str("#37;"),
                _ => out.push(c),
            }
        }
    }
    out
}

/// Cuts an escaped label to [`LABEL_MAX`] characters at an entity boundary:
/// the cut never lands inside a `#...;`, so no entity is left half-written.
fn cut(escaped: &str) -> String {
    if escaped.chars().count() <= LABEL_MAX {
        return escaped.to_string();
    }
    let mut out: String = escaped.chars().take(LABEL_MAX).collect();
    // Every `#` in an escaped label starts an entity; drop a trailing one
    // that lost its `;`.
    while let Some(pos) = out.rfind('#') {
        if out[pos..].contains(';') {
            break;
        }
        out.truncate(pos);
    }
    out
}

/// The diagram text of a version whose YAML is `yaml`; an
/// `InvalidData` error when the YAML does not parse.
pub fn text_of(yaml: &str) -> std::io::Result<String> {
    let playbook = Playbook::from_yaml(yaml)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(mermaid(&playbook, &crate::scope::digest_str(yaml)))
}

/// Writes `playbook.mmd` into `version_dir` from `yaml` (already validated),
/// atomically. For a version directory the caller just made (a new
/// version, a staged copy); a rebuild of stored versions writes through
/// `fsutil::beneath` instead, so no link in the store is followed.
pub fn write_beside(version_dir: &Path, yaml: &str) -> std::io::Result<()> {
    let text = text_of(yaml)?;
    crate::fsutil::atomic_write(&version_dir.join(DIAGRAM_FILE), text.as_bytes())
}

/// How the diagram of one version directory stands against its YAML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramState {
    /// The file exists and renders exactly what the YAML renders today.
    Current,
    /// The version has no `playbook.mmd`.
    Missing,
    /// The file's digest is not the digest of the YAML beside it.
    Stale,
    /// The digest matches but the text was changed by hand (or the entry
    /// is not a regular file). Seen only between writes: the next repair
    /// or rebuild overwrites it.
    Edited,
    /// The version's YAML is absent or does not parse, so nothing can be
    /// said.
    Unreadable,
}

/// The largest `playbook.mmd` `state_of` reads; a larger one reads as
/// edited and is written again.
const READ_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The cap of a version's `playbook.yaml` read for its diagram: none, the
/// policy every version load follows (`registry`), so a stored version
/// that loads is never refused a diagram, nor a move of `current`, for its
/// size alone. The read is still beneath the store, of a regular file with
/// one link, never through a link.
pub const YAML_READ_MAX_BYTES: u64 = u64::MAX;

/// How the diagram of one version directory stands against its YAML.
/// Both files are read beneath `version_dir` (`fsutil::beneath`): a link,
/// a FIFO or a device is never followed or opened. A `playbook.yaml` that
/// is not a regular file is `Unreadable`; a `playbook.mmd` that is not one
/// is `Edited` (apb writes a regular file there, and the next repair or
/// rebuild replaces it).
pub fn state_of(version_dir: &Path) -> DiagramState {
    let Ok(dir) = crate::fsutil::beneath::Beneath::open(version_dir) else {
        return DiagramState::Unreadable;
    };
    inspect(&dir, "").0
}

/// Gives version `version` of the playbook directory `playbook_dir` the
/// diagram derived from exactly the `playbook.yaml` bytes it holds,
/// whatever sits at `playbook.mmd`: a missing, stale or edited diagram is
/// written again (the file is derived; neither a person nor an agent
/// maintains it, so an edit is overwritten), and an entry that is not a
/// regular file is replaced without following it ([`replace_beside`]). An
/// `Unreadable` version (its YAML absent, not a regular file or not
/// parsing, or a playbook directory that is a link) has no diagram to
/// derive and is left as it is. Read and written beneath `playbook_dir`,
/// so no link is followed. The state the version is in afterwards. This is
/// the repair `current::move_current` runs before `current` names a
/// version, so a version an interrupted write, an older apb or a hand edit
/// left without its diagram is completed before anything points at it.
pub fn ensure_beside(playbook_dir: &Path, version: &str) -> std::io::Result<DiagramState> {
    // A playbook directory that is a link is not opened: nothing is
    // derived through it, and `apb doctor` names its diagrams.
    let Ok(dir) = crate::fsutil::beneath::Beneath::open(playbook_dir) else {
        return Ok(DiagramState::Unreadable);
    };
    let (state, text) = inspect(&dir, &format!("{version}/"));
    match (state, text) {
        (DiagramState::Missing | DiagramState::Stale | DiagramState::Edited, Some(text)) => {
            replace_beside(&dir, version, &text)?;
            Ok(DiagramState::Current)
        }
        (state, _) => Ok(state),
    }
}

/// Writes `text` as `<version_rel>/playbook.mmd` beneath `dir`, whatever
/// sits at that name but a directory. A fresh regular file is written
/// under a temporary name in the same directory and renamed over the
/// entry: a regular file, a link (replaced itself, never what it points
/// at), a FIFO or a device. No moment of the repair leaves the name
/// without an entry. A directory at that name, empty or not, is an error
/// that names the file and says what to do. The one write of a stored version's diagram: the repair
/// before `current` moves and `apb diagram --rebuild`.
pub fn replace_beside(
    dir: &crate::fsutil::beneath::Beneath,
    version_rel: &str,
    text: &str,
) -> std::io::Result<()> {
    use crate::fsutil::beneath::EntryKind;
    let rel = format!("{version_rel}/{DIAGRAM_FILE}");
    let explain = |e: std::io::Error| {
        std::io::Error::new(
            e.kind(),
            format!(
                "{rel}: apb derives this file from playbook.yaml and cannot replace what sits at that name ({e}); remove it and try again"
            ),
        )
    };
    // A directory is never removed: a file renamed over it fails, and
    // emptying it is the person's call. Every other entry (a link, a FIFO,
    // a device) is replaced by the rename itself, which never follows a
    // link, so the name holds an entry at every moment of the repair.
    if dir.kind(&rel).map_err(explain)? == Some(EntryKind::Dir) {
        return Err(explain(std::io::Error::new(
            std::io::ErrorKind::IsADirectory,
            "a directory",
        )));
    }
    dir.write_new(version_rel, DIAGRAM_FILE, text.as_bytes())
        .map_err(explain)
}

/// The state of the version whose files sit at `<prefix>playbook.yaml` and
/// `<prefix>playbook.mmd` beneath `dir`, with the diagram text derived
/// from the YAML bytes read (`None` when they do not parse).
fn inspect(dir: &crate::fsutil::beneath::Beneath, prefix: &str) -> (DiagramState, Option<String>) {
    use crate::fsutil::beneath::{CappedRead, ReadRefusal};
    let yaml = match dir.read_regular_capped(&format!("{prefix}playbook.yaml"), YAML_READ_MAX_BYTES)
    {
        Ok(CappedRead::Read { bytes, .. }) => match String::from_utf8(bytes) {
            Ok(yaml) => yaml,
            Err(_) => return (DiagramState::Unreadable, None),
        },
        _ => return (DiagramState::Unreadable, None),
    };
    let playbook = match Playbook::from_yaml(&yaml) {
        Ok(playbook) => playbook,
        Err(_) => return (DiagramState::Unreadable, None),
    };
    let digest = crate::scope::digest_str(&yaml);
    let derived = mermaid(&playbook, &digest);
    let state = match dir.read_regular_capped(&format!("{prefix}{DIAGRAM_FILE}"), READ_MAX_BYTES) {
        Ok(CappedRead::Read { bytes, .. }) => match String::from_utf8(bytes) {
            Ok(text) => {
                let recorded = text
                    .lines()
                    .next()
                    .and_then(|first| first.strip_prefix(DIGEST_PREFIX))
                    .and_then(|rest| rest.split_whitespace().next());
                if recorded != Some(digest.as_str()) {
                    DiagramState::Stale
                } else if text != derived {
                    DiagramState::Edited
                } else {
                    DiagramState::Current
                }
            }
            Err(_) => DiagramState::Edited,
        },
        Ok(CappedRead::Refused(ReadRefusal::Missing)) => DiagramState::Missing,
        Ok(CappedRead::Refused(_)) => DiagramState::Edited,
        Err(_) => DiagramState::Missing,
    };
    (state, Some(derived))
}

/// Counts over every version directory under `playbooks_dir`
/// (`<dir>/<id>/<version>/playbook.yaml`). Unreadable version directories
/// have no field: they are skipped, never counted as any state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagramScan {
    pub current: u32,
    pub missing: u32,
    pub stale: u32,
    pub edited: u32,
}

/// Counts the diagram states of every version directory under
/// `playbooks_dir`.
pub fn scan(playbooks_dir: &Path) -> DiagramScan {
    let mut out = DiagramScan::default();
    let Ok(ids) = fs::read_dir(playbooks_dir) else {
        return out;
    };
    for id in ids.flatten() {
        if !is_dir(&id.path()) {
            continue;
        }
        let Ok(versions) = fs::read_dir(id.path()) else {
            continue;
        };
        for version in versions.flatten() {
            let dir = version.path();
            if !is_dir(&dir) || !dir.join("playbook.yaml").is_file() {
                continue;
            }
            match state_of(&dir) {
                DiagramState::Current => out.current += 1,
                DiagramState::Missing => out.missing += 1,
                DiagramState::Stale => out.stale += 1,
                DiagramState::Edited => out.edited += 1,
                DiagramState::Unreadable => {}
            }
        }
    }
    out
}

/// Whether `path` is a directory this process may walk, never following a
/// link.
fn is_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.is_dir())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = "schema: 2\nid: demo\nname: Demo\nversion: 1.0.0\nnodes:\n  - { id: start, type: start, title: Start }\n  - id: ask\n    type: human_review\n    title: Ask?\n    options: [approve, reject]\n  - { id: done, type: finish, outcome: success, title: Done }\nedges:\n  - { from: start, to: ask }\n  - { from: ask, to: done, condition: { type: review_status, equals: approve } }\n";

    /// One node of every shape group and one edge of every label kind, in
    /// node order: s=0, at=1, sc=2, pr=3, cd=4, hr=5, wt=6, fn=7, sub=8,
    /// jg=9, pu=10.
    const SHAPES: &str = "schema: 2\nid: shapes\nname: Shapes\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: at, type: agent_task, prompt: do it }\n  - { id: sc, type: script, script: x.sh, runner: sh }\n  - { id: pr, type: prompt, prompt: hello }\n  - { id: cd, type: condition }\n  - { id: hr, type: human_review, options: ['a|b'] }\n  - { id: wt, type: wait, wait_for: { type: timer, seconds: 5 }, timeout_seconds: 60 }\n  - { id: fn, type: finish, outcome: success }\n  - { id: sub, type: playbook, playbook: child-pb }\n  - { id: jg, type: judge }\n  - { id: pu, type: publish, from: at, remote: origin }\nedges:\n  - { from: s, to: at }\n  - { from: s, to: sc, condition: { type: review_status, equals: 'a|b' } }\n  - { from: s, to: pr, condition: { type: output_field, node: at, field: score, gte: 80 } }\n  - { from: s, to: cd, condition: { type: output_field, node: at, field: verdict, equals: ok } }\n  - { from: s, to: hr, condition: { type: output_match, node: at, pattern: \"ok*\" } }\n  - { from: s, to: wt, condition: { type: node_status, node: at, equals: failure } }\n  - { from: s, to: fn, condition: { type: node_status, node: at, equals: stalled } }\n  - { from: s, to: sub, condition: { type: judge, question: \"Is it good?\", min_p: 0.8, on_unavailable: false } }\n  - { from: s, to: jg, condition: { type: no_new_items, node: at, field: findings, rounds: 2 } }\n  - { from: s, to: pu, join: all }\n  - { from: sc, to: s, join: any }\n  - { from: s, to: at, max_traversals: 3 }\n";

    fn pb(yaml: &str) -> Playbook {
        Playbook::from_yaml(yaml).unwrap()
    }

    /// A stored version larger than the diagram file's own cap still gets
    /// its diagram: the YAML is read under the version-loading policy, so
    /// a move of `current` is never refused for the definition's size.
    #[test]
    fn a_large_definition_gets_its_diagram() {
        let tmp = tempfile::tempdir().unwrap();
        let version = tmp.path().join("1.0.0");
        std::fs::create_dir_all(&version).unwrap();
        let padding = format!("# {}\n", "x".repeat(1022)).repeat(9 * 1024);
        let yaml = format!("{padding}{SIMPLE}");
        assert!(yaml.len() as u64 > READ_MAX_BYTES);
        std::fs::write(version.join("playbook.yaml"), &yaml).unwrap();
        assert_eq!(
            ensure_beside(tmp.path(), "1.0.0").unwrap(),
            DiagramState::Current
        );
    }

    #[test]
    fn the_text_is_stable() {
        let playbook = pb(SIMPLE);
        let first = mermaid(&playbook, "sha256:abc");
        // Definition order: start, the review gate, the finish.
        let i0 = first.find("n0([\"Start (start)\"])").unwrap();
        let i1 = first.find("n1{\"Ask? (human_review)\"}").unwrap();
        let i2 = first.find("n2([\"Done (finish success)\"])").unwrap();
        assert!(i0 < i1 && i1 < i2, "nodes follow definition order: {first}");
        // Edge order: the plain edge before the labelled one.
        let e0 = first.find("  n0 --> n1\n").unwrap();
        let e1 = first.find("  n1 -->|\"approve\"| n2\n").unwrap();
        assert!(e0 < e1, "edges follow declaration order: {first}");
        assert_eq!(
            first.lines().next().unwrap(),
            "%% apb-diagram sha256:abc derived from playbook.yaml; apb diagram --rebuild writes it again"
        );
        assert_eq!(first.lines().nth(1).unwrap(), "flowchart TD");
    }

    #[test]
    fn shapes_and_edge_labels() {
        let text = mermaid(&pb(SHAPES), "sha256:abc");
        // Shapes: start and finish are stadium, gates are rhombus, the rest
        // rectangles.
        assert!(text.contains("n0([\"s (start)\"])"), "{text}");
        assert!(text.contains("n1[\"at (agent_task)\"]"), "{text}");
        assert!(text.contains("n2[\"sc (script)\"]"), "{text}");
        assert!(text.contains("n3[\"pr (prompt)\"]"), "{text}");
        assert!(text.contains("n4{\"cd (condition)\"}"), "{text}");
        assert!(text.contains("n5{\"hr (human_review)\"}"), "{text}");
        assert!(text.contains("n6[\"wt (wait)\"]"), "{text}");
        assert!(text.contains("n7([\"fn (finish success)\"])"), "{text}");
        assert!(text.contains("n8[\"sub (playbook)\"]"), "{text}");
        assert!(text.contains("n9{\"jg (judge)\"}"), "{text}");
        assert!(text.contains("n10{\"pu (publish)\"}"), "{text}");
        // Edge labels, one per kind; the plain edge carries none.
        assert!(text.contains("  n0 --> n1\n"), "{text}");
        assert!(text.contains("  n0 -->|\"a#124;b\"| n2\n"), "{text}");
        assert!(text.contains("  n0 -->|\"score #gt;= 80\"| n3\n"), "{text}");
        assert!(text.contains("  n0 -->|\"verdict = ok\"| n4\n"), "{text}");
        assert!(text.contains("  n0 -->|\"matches ok*\"| n5\n"), "{text}");
        assert!(text.contains("  n0 -->|\"at failure\"| n6\n"), "{text}");
        assert!(text.contains("  n0 -->|\"at stalled\"| n7\n"), "{text}");
        assert!(
            text.contains("  n0 -->|\"judge: Is it good?\"| n8\n"),
            "{text}"
        );
        assert!(
            text.contains("  n0 -->|\"no new findings\"| n9\n"),
            "{text}"
        );
        assert!(text.contains("  n0 -->|\"join all\"| n10\n"), "{text}");
        assert!(text.contains("  n2 -->|\"join any\"| n0\n"), "{text}");
        assert!(text.contains("  n0 -->|\"max 3\"| n1\n"), "{text}");
    }

    #[test]
    fn author_text_never_leaves_its_label() {
        // The title carries a real line break, quotes, `]`, click-shaped
        // text and a Mermaid init directive; the option and the review
        // status carry a `|`.
        let yaml = "schema: 2\nid: nasty\nname: Nasty\nversion: 1.0.0\nnodes:\n  - id: n0\n    type: agent_task\n    prompt: go\n    title: \"x\\\"] click n0 \\\"https://e\\\" \\n%%{init: {}}%%\"\n  - { id: gate, type: human_review, options: ['a|b'] }\n  - { id: fin, type: finish, outcome: success }\nedges:\n  - { from: n0, to: gate }\n  - { from: gate, to: fin, condition: { type: review_status, equals: 'a|b' } }\n";
        let text = mermaid(&pb(yaml), "sha256:abc");
        for line in text.lines() {
            assert!(
                line.starts_with("  n")
                    || line.starts_with("flowchart")
                    || line.starts_with("%% apb-diagram"),
                "a line starts outside apb's shapes: {line:?}"
            );
        }
        // No `click` outside quotes: strip every quoted span (a `"` in the
        // output is always a real delimiter; author quotes became
        // `#quot;`), then look.
        let mut outside = String::new();
        let mut quoted = false;
        for c in text.chars() {
            match c {
                '"' => quoted = !quoted,
                _ if !quoted => outside.push(c),
                _ => {}
            }
        }
        assert!(
            !outside.contains("click"),
            "`click` leaked out of a label: {outside:?}"
        );
        assert!(
            !outside.contains("%%{") && !outside.contains("init:"),
            "a directive leaked out of a label: {outside:?}"
        );
        // Mermaid looks for directives before it parses quoted labels, so
        // no `%` of author text may survive even inside one: the only `%`
        // of the text is apb's own header comment.
        for line in text.lines().filter(|l| !l.starts_with("%% apb-diagram")) {
            assert!(!line.contains('%'), "a `%` survived in a label: {line:?}");
        }
        assert!(text.contains("#37;#37;{init: {}}"), "{text}");
    }

    #[test]
    fn the_diagram_changes_no_digest() {
        let dir = tempfile::tempdir().unwrap();
        let vdir = dir.path().join("demo").join("1.0.0");
        fs::create_dir_all(vdir.join("scripts")).unwrap();
        fs::write(vdir.join("playbook.yaml"), SIMPLE).unwrap();
        // A script, so the trust digest walks files beside the YAML, not
        // the YAML alone.
        fs::write(vdir.join("scripts/work.sh"), "echo hi\n").unwrap();
        let before = crate::scope::definition_digest(SIMPLE, &vdir).unwrap();
        assert_ne!(before, crate::scope::digest_str(SIMPLE));
        write_beside(&vdir, SIMPLE).unwrap();
        assert!(vdir.join(DIAGRAM_FILE).is_file());
        let written = crate::scope::definition_digest(SIMPLE, &vdir).unwrap();
        assert_eq!(before, written, "playbook.mmd must sit outside the digest");
        // An edited diagram changes nothing either.
        fs::write(vdir.join(DIAGRAM_FILE), "flowchart TD\n  edited\n").unwrap();
        let edited = crate::scope::definition_digest(SIMPLE, &vdir).unwrap();
        assert_eq!(before, edited, "an edited playbook.mmd must not move it");
    }

    #[test]
    fn state_of_tells_each_case() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("playbooks");
        let current = store.join("a").join("1.0.0");
        let missing = store.join("a").join("1.1.0");
        let stale = store.join("b").join("1.0.0");
        let edited = store.join("b").join("1.1.0");
        let broken = store.join("c").join("1.0.0");
        for v in [&current, &missing, &stale, &edited, &broken] {
            fs::create_dir_all(v).unwrap();
        }
        fs::write(current.join("playbook.yaml"), SIMPLE).unwrap();
        write_beside(&current, SIMPLE).unwrap();
        fs::write(missing.join("playbook.yaml"), SIMPLE).unwrap();
        fs::write(stale.join("playbook.yaml"), SIMPLE).unwrap();
        write_beside(&stale, SIMPLE).unwrap();
        let changed = SIMPLE.replace("name: Demo", "name: Changed");
        fs::write(stale.join("playbook.yaml"), &changed).unwrap();
        fs::write(edited.join("playbook.yaml"), SIMPLE).unwrap();
        write_beside(&edited, SIMPLE).unwrap();
        let mmd = edited.join(DIAGRAM_FILE);
        let rendered = fs::read_to_string(&mmd).unwrap();
        fs::write(&mmd, rendered.replace("flowchart TD", "flowchart LR")).unwrap();
        fs::write(broken.join("playbook.yaml"), ": not: [a: playbook").unwrap();

        assert_eq!(state_of(&current), DiagramState::Current);
        assert_eq!(state_of(&missing), DiagramState::Missing);
        assert_eq!(state_of(&stale), DiagramState::Stale);
        assert_eq!(state_of(&edited), DiagramState::Edited);
        // No playbook.yaml at all, and one that does not parse.
        assert_eq!(
            state_of(&store.join("c").join("9.9.9")),
            DiagramState::Unreadable
        );
        assert_eq!(state_of(&broken), DiagramState::Unreadable);
    }

    /// A `playbook.mmd` or a `playbook.yaml` that is a link (here to a
    /// file that would read as current) is never followed: the diagram
    /// is not one apb wrote, and a YAML read through a link says nothing
    /// about this version. A FIFO in its place is never opened either.
    #[test]
    fn state_of_follows_no_link() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("playbook.yaml"), SIMPLE).unwrap();
        write_beside(&elsewhere, SIMPLE).unwrap();
        let linked_mmd = dir.path().join("a").join("1.0.0");
        fs::create_dir_all(&linked_mmd).unwrap();
        fs::write(linked_mmd.join("playbook.yaml"), SIMPLE).unwrap();
        std::os::unix::fs::symlink(elsewhere.join(DIAGRAM_FILE), linked_mmd.join(DIAGRAM_FILE))
            .unwrap();
        assert_eq!(state_of(&linked_mmd), DiagramState::Edited);
        let linked_yaml = dir.path().join("a").join("1.1.0");
        fs::create_dir_all(&linked_yaml).unwrap();
        std::os::unix::fs::symlink(
            elsewhere.join("playbook.yaml"),
            linked_yaml.join("playbook.yaml"),
        )
        .unwrap();
        write_beside(&linked_yaml, SIMPLE).unwrap();
        assert_eq!(state_of(&linked_yaml), DiagramState::Unreadable);
        let fifo = dir.path().join("a").join("1.2.0");
        fs::create_dir_all(&fifo).unwrap();
        fs::write(fifo.join("playbook.yaml"), SIMPLE).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(fifo.join(DIAGRAM_FILE))
            .status()
            .unwrap();
        assert!(made.success());
        assert_eq!(state_of(&fifo), DiagramState::Edited);
    }

    /// What sits at `playbook.mmd` and is not a regular file (a link, a
    /// FIFO) is replaced by one rename over it: at every moment of the
    /// repair the name holds an entry, a link is replaced itself and what
    /// it points at stays as it was. A directory at that name is refused
    /// with a message that names the file, and stays.
    #[test]
    fn replace_beside_never_leaves_the_name_empty() {
        let t = tempfile::tempdir().unwrap();
        let outside = t.path().join("outside.mmd");
        fs::write(&outside, "kept\n").unwrap();
        let store = t.path().join("p");
        let version = store.join("1.0.0");
        fs::create_dir_all(&version).unwrap();
        let mmd = version.join(DIAGRAM_FILE);
        let dir = crate::fsutil::beneath::Beneath::open(&store).unwrap();
        let replace_watched = || {
            let seen = std::rc::Rc::new(std::cell::Cell::new(None));
            let (watch, at) = (seen.clone(), mmd.clone());
            crate::fsutil::beneath::tests::plant_between(move || {
                watch.set(Some(fs::symlink_metadata(&at).is_ok()));
            });
            replace_beside(&dir, "1.0.0", "derived\n").unwrap();
            assert_eq!(
                seen.get(),
                Some(true),
                "the name held an entry while the new file was written"
            );
            assert!(fs::symlink_metadata(&mmd).unwrap().file_type().is_file());
            assert_eq!(fs::read_to_string(&mmd).unwrap(), "derived\n");
        };
        std::os::unix::fs::symlink(&outside, &mmd).unwrap();
        replace_watched();
        assert_eq!(fs::read_to_string(&outside).unwrap(), "kept\n");
        fs::remove_file(&mmd).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&mmd)
            .status()
            .unwrap();
        assert!(made.success());
        replace_watched();
        fs::remove_file(&mmd).unwrap();
        fs::create_dir(&mmd).unwrap();
        let refused = replace_beside(&dir, "1.0.0", "derived\n")
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("1.0.0/playbook.mmd") && refused.contains("remove it and try again"),
            "{refused}"
        );
        assert!(mmd.is_dir());
    }

    #[test]
    fn scan_counts_every_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("playbooks");
        let current = store.join("a").join("1.0.0");
        let stale = store.join("a").join("1.1.0");
        let missing = store.join("b").join("2.0.0");
        let edited = store.join("b").join("2.1.0");
        let stray = store.join("b").join("meta");
        for v in [&current, &stale, &missing, &edited, &stray] {
            fs::create_dir_all(v).unwrap();
        }
        fs::write(current.join("playbook.yaml"), SIMPLE).unwrap();
        write_beside(&current, SIMPLE).unwrap();
        // Stale: the diagram of the old text beside a changed YAML.
        fs::write(stale.join("playbook.yaml"), SIMPLE).unwrap();
        write_beside(&stale, SIMPLE).unwrap();
        fs::write(
            stale.join("playbook.yaml"),
            SIMPLE.replace("name: Demo", "name: V2"),
        )
        .unwrap();
        fs::write(missing.join("playbook.yaml"), SIMPLE).unwrap();
        fs::write(edited.join("playbook.yaml"), SIMPLE).unwrap();
        write_beside(&edited, SIMPLE).unwrap();
        // A hand edit: the digest line stands, the body changed (a first
        // line that lost its digest would read as stale, not edited).
        let rendered = fs::read_to_string(edited.join(DIAGRAM_FILE)).unwrap();
        fs::write(
            edited.join(DIAGRAM_FILE),
            rendered.replace("flowchart TD", "flowchart LR"),
        )
        .unwrap();
        fs::write(stray.join("notes.txt"), "not a version").unwrap();

        assert_eq!(
            scan(&store),
            DiagramScan {
                current: 1,
                missing: 1,
                stale: 1,
                edited: 1
            }
        );
        // A store that does not exist counts nothing.
        assert_eq!(scan(&dir.path().join("nowhere")), DiagramScan::default());
    }
}
