//! The write class of a concurrent batch member and the rule that decides
//! which members a batch admits (spec loops-and-graphs D1): a member that
//! only reads (the detected tier) may batch, but only beside members that
//! cannot write, so a change its check sees was made by a co-member that
//! ran at the same time. Kept out of `scheduler/node.rs`, which is already
//! over 4600 lines.

use apb_core::schema::Playbook;

/// How a batchable node can move the tree while it runs.
#[derive(PartialEq, Eq)]
pub(crate) enum WriteClass {
    /// A batchable node that is not `read_only`: it may write.
    Writer,
    /// `read_only: true` with `isolation: full`: the sandbox binds the tree
    /// read-only, a write fails at the OS level, so the node cannot move
    /// the tree and may batch with anything.
    Enforced,
    /// `read_only: true` in any other tier: the engine detects a change by
    /// comparing the tree before and after the attempt, so it fails beside
    /// a peer that wrote and may only batch with members that cannot write.
    Detected,
}

/// The write class of `node`: `Enforced` when it is read-only under
/// `isolation: full`, `Detected` when it is read-only in any other tier
/// (no sandbox, or a sandbox that is not full), `Writer` otherwise.
pub(crate) fn write_class(playbook: &Playbook, node: &str) -> WriteClass {
    let Some(n) = playbook.node(node) else {
        return WriteClass::Writer;
    };
    if detected_read_only(n) {
        WriteClass::Detected
    } else if n.read_only {
        WriteClass::Enforced
    } else {
        WriteClass::Writer
    }
}

/// Whether `batch` (the whole logical batch, finished members included)
/// admits `candidate` as a member: a detected read-only member never
/// batches with one that may write, in either direction; an enforced
/// member mixes with both.
pub(crate) fn batch_admits(playbook: &Playbook, batch: &[String], candidate: &str) -> bool {
    let is_detected = |n: &str| write_class(playbook, n) == WriteClass::Detected;
    let is_writer = |n: &str| write_class(playbook, n) == WriteClass::Writer;
    let candidate_scoped = is_scoped(playbook, candidate);
    let batch_scoped = batch.iter().any(|m| is_scoped(playbook, m));
    // A scoped writer never shares a batch with any other writer, scoped or
    // not: a whole-tree comparison cannot tell which process made a change,
    // so one writer's stray write could pass as the other's in-scope one.
    if candidate_scoped && batch.iter().any(|m| is_writer(m)) {
        return false;
    }
    if batch_scoped && write_class(playbook, candidate) == WriteClass::Writer {
        return false;
    }
    match write_class(playbook, candidate) {
        WriteClass::Enforced => true,
        WriteClass::Writer => !batch.iter().any(|m| is_detected(m)),
        WriteClass::Detected => !batch.iter().any(|m| is_writer(m)),
    }
}

/// Whether the node declares a write scope (`writes` or
/// `max_changed_files`): its check blames the whole tree, so it runs alone
/// among writers.
pub(crate) fn is_scoped(playbook: &Playbook, node_id: &str) -> bool {
    playbook
        .node(node_id)
        .is_some_and(|n| n.writes.is_some() || n.max_changed_files.is_some())
}

/// Whether `node` is read-only in a tier that may be the detected one.
fn detected_read_only(node: &apb_core::schema::Node) -> bool {
    node.read_only
        && node
            .kind
            .isolation()
            .is_none_or(|i| i.level != apb_core::isolation::Level::Full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playbook() -> Playbook {
        let yaml = r#"
schema: 2
id: t
name: T
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: s, runner: sh }
  - { id: d, type: script, script: s, runner: sh, read_only: true }
  - { id: e, type: agent_task, prompt: p, read_only: true, isolation: { level: full } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;
        yaml_serde::from_str(yaml).unwrap()
    }

    fn batch(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn the_write_class_of_a_node_follows_read_only_and_isolation() {
        let pb = playbook();
        assert!(matches!(write_class(&pb, "w"), WriteClass::Writer));
        assert!(matches!(write_class(&pb, "d"), WriteClass::Detected));
        assert!(matches!(write_class(&pb, "e"), WriteClass::Enforced));
    }

    #[test]
    fn a_detected_member_never_batches_with_a_writer_in_either_direction() {
        let pb = playbook();
        // Either direction refuses.
        assert!(!batch_admits(&pb, &batch(&["d"]), "w"));
        assert!(!batch_admits(&pb, &batch(&["w"]), "d"));
        // Two detected members batch, and so do two writers.
        assert!(batch_admits(&pb, &batch(&["d"]), "d"));
        assert!(batch_admits(&pb, &batch(&["w"]), "w"));
        // An enforced member mixes with both, and admits anything beside it.
        assert!(batch_admits(&pb, &batch(&["w"]), "e"));
        assert!(batch_admits(&pb, &batch(&["d"]), "e"));
        assert!(batch_admits(&pb, &batch(&["e"]), "w"));
        assert!(batch_admits(&pb, &batch(&["e"]), "d"));
    }

    /// A scoped candidate is refused beside a writer, a writer beside a
    /// scoped member; a scoped node beside an enforced read-only one is
    /// admitted (spec node-contracts D3).
    #[test]
    fn a_scoped_node_never_batches_with_another_writer() {
        let pb = playbook();
        assert!(!is_scoped(&pb, "w"));
        // A scoped candidate beside a writer.
        let pb = scoped_playbook("w");
        assert!(is_scoped(&pb, "w"));
        assert!(!batch_admits(&pb, &batch(&["w2"]), "w"));
        // A writer beside a scoped member.
        assert!(!batch_admits(&pb, &batch(&["w"]), "w2"));
        // Two scoped members are still refused: neither is the other's alibi.
        assert!(!batch_admits(&pb, &batch(&["w"]), "w3"));
        // A scoped node beside an enforced read-only one mixes.
        let both = scoped_playbook("s1");
        assert!(batch_admits(&both, &batch(&["e"]), "s1"));
        assert!(batch_admits(&both, &batch(&["s1"]), "e"));
        // Unscoped writers keep today's behaviour.
        let plain = playbook();
        assert!(!batch_admits(&plain, &batch(&["w"]), "d"));
    }

    fn scoped_playbook(scoped_id: &str) -> Playbook {
        let yaml = format!(
            r#"
schema: 2
id: t
name: T
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - {{ id: {scoped_id}, type: script, script: s, runner: sh, writes: ["src/**"] }}
  - {{ id: w2, type: script, script: s, runner: sh }}
  - {{ id: w3, type: script, script: s, runner: sh, max_changed_files: 3 }}
  - {{ id: e, type: agent_task, prompt: p, read_only: true, isolation: {{ level: full }} }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: {scoped_id} }}
  - {{ from: {scoped_id}, to: done }}
"#
        );
        yaml_serde::from_str(&yaml).unwrap()
    }

    /// The rule weighs the whole logical batch: a member that already
    /// finished still keeps a writer out of a batch that holds a detected
    /// member, and keeps a detected candidate out of one that holds a
    /// writer.
    #[test]
    fn the_rule_weighs_finished_members_too() {
        let pb = playbook();
        assert!(!batch_admits(&pb, &batch(&["w", "d"]), "w"));
        assert!(!batch_admits(&pb, &batch(&["d", "w"]), "d"));
    }
}
