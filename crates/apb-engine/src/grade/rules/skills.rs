//! `skill_use`: whether each skill a node's profile gives it was read, and
//! whether a read skill served its node.
//!
//! A read shows in claude's session as a `Skill` tool call or a read of the
//! skill's `SKILL.md`. Other agents keep no session apb can read, so their
//! reads are `unknown`. A skill no attempt read is not judged
//! (`not_applicable`); a read one is judged by its node: succeeded at once
//! (good), after a failed attempt (partial), or failed (poor).

use std::collections::BTreeMap;

use apb_core::quality::grade::{Level, SkillRead, SkillReadState};

use super::finding::Finding;
use crate::event::EventPayload;
use crate::grade::sources::RunSources;
use crate::state::{NodeStatus, RunState};

/// Whether each skill of each bound node was read.
pub(super) fn reads(sources: &RunSources) -> Vec<SkillRead> {
    let Some(manifest) = &sources.run.manifest else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (node, key) in &manifest.node_bindings {
        let Some(profile) = manifest.profiles.iter().find(|p| p.key() == *key) else {
            continue;
        };
        let attempts: Vec<_> = sources
            .transcripts
            .iter()
            .filter(|t| t.source.node == *node)
            .collect();
        if attempts.is_empty() {
            continue;
        }
        let stems: std::collections::BTreeSet<&str> =
            attempts.iter().map(|t| t.source.stem.as_str()).collect();
        let unseen = stems.iter().any(|stem| {
            !attempts
                .iter()
                .any(|t| t.source.stem == *stem && t.source.structured() && t.complete)
        });
        for skill in &profile.skills {
            let at = attempts
                .iter()
                .filter(|t| t.source.structured())
                .find_map(|t| {
                    t.records
                        .iter()
                        .find(|r| {
                            r.tools
                                .iter()
                                .any(|u| u.skill.as_deref() == Some(&skill.name))
                        })
                        .map(|r| t.record_ref(r.index))
                });
            let read = match (&at, unseen) {
                (Some(_), _) => SkillReadState::Yes,
                (None, true) => SkillReadState::Unknown,
                (None, false) => SkillReadState::No,
            };
            out.push(SkillRead {
                node: node.clone(),
                profile: key.clone(),
                skill: skill.name.clone(),
                bundle_digest: profile.bundle_digest.clone(),
                read,
                at,
            });
        }
    }
    out
}

pub(super) fn grade(sources: &RunSources, reads: &[SkillRead]) -> Finding {
    let read: Vec<&SkillRead> = reads
        .iter()
        .filter(|r| r.read == SkillReadState::Yes)
        .collect();
    if read.is_empty() {
        return if reads.iter().any(|r| r.read == SkillReadState::Unknown) {
            Finding::new(
                Level::NotEnoughData,
                "skill_read_unknown",
                "whether a skill was read cannot be told from the attempts' records",
            )
        } else {
            Finding::new(Level::NotApplicable, "no_skill_read", "no skill was read")
        };
    }
    let state = RunState::fold(&sources.run.events);
    let mut failed: BTreeMap<&str, u32> = BTreeMap::new();
    for e in &sources.run.events {
        if let EventPayload::AttemptFinished { node, status, .. } = &e.payload
            && status != "succeeded"
        {
            *failed.entry(node).or_default() += 1;
        }
    }
    let level_of = |node: &str| match state.nodes.get(node) {
        Some(NodeStatus::Succeeded) if failed.get(node).is_none_or(|n| *n == 0) => Level::Good,
        Some(NodeStatus::Succeeded) => Level::Partial,
        _ => Level::Poor,
    };
    let level = read
        .iter()
        .map(|r| level_of(&r.node))
        .max()
        .unwrap_or(Level::Good);
    let (code, why) = match level {
        Level::Good => (
            "skill_served",
            "every skill read served a node that succeeded",
        ),
        Level::Partial => (
            "skill_node_recovered",
            "a skill was read and its node succeeded only after a failure",
        ),
        _ => ("skill_node_failed", "a skill was read and its node failed"),
    };
    Finding::new(level, code, why).citing(read.iter().filter_map(|r| r.at.clone()).collect())
}
