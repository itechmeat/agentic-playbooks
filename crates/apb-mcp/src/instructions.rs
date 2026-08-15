//! Tier 0 (spec 4): static behavior rules baked into
//! `ServerInfo.instructions`. Only our trusted text - no project data
//! (injection hygiene). The catalog and details are pulled by tools.

/// Byte budget for the instructions field: Claude Code truncates a server's
/// instructions at 2KB, so the shipped text stays under this cap (pinned by a
/// unit test, see docs/HOST-INTEGRATION.md).
pub const TIER0_MAX_BYTES: usize = 1950;

pub const TIER0: &str = "\
Discovery: call playbook_catalog once per task that names a doable action, before acting. Skip chit-chat. It returns trigger, effects, trust and suppressed_suggestions.

Offering to save: if you just completed a multi-step repeatable action, or the user asks for one recurring by nature, and no playbook matched, you MUST offer once to save it with playbook_capture: one short question offering project or global scope, project first if project-specific. First compare it with suppressed_suggestions by synopsis meaning, not slug (empty synopsis: by slug); a covering record means no offer. One offer per session.

Declines: when the user declines without saying never, call suggestion_dismiss with kind soft, project scope and a one-sentence synopsis. Reserve kind hard for an explicit never-again, global scope for everywhere-wording. Never ask about scope.

Interview: when the user describes a process to automate, offer a short interview and pull playbook_interview.

Using a match: on a confident match to an active, trusted playbook, name it in one line and run it. One short question if ambiguous; confirm first for another project.

Running policy: the server refuses drafts and untrusted playbooks until trial or acknowledgement. Effects beyond the request (network, secrets, deploys, irreversible) need confirmation.

Human gates: run_status, supervisor_wait_event and supervisor_run_inspect return pending_review at a human_review gate. Relay its instruction in the user's language with the options, then record it with review_decide. Frozen until then; repeat while pending.

Profiles: a node binds its executor only through a profile (agent, model, fallbacks, role prompt, skills). Call profile_list to reuse one, profile_howto for format.

Lifecycle: update, clone, version and delete playbooks; pull playbook_howto when authoring. Call projects_list for another workspace. Machine fields are English; speak the user's language.";

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code truncates a server's `instructions` at 2KB (measured, July
    /// 2026, see docs/HOST-INTEGRATION.md), so the text has a hard budget. A
    /// silent overrun would drop the tail of the text rather than fail, which
    /// is why this is pinned by a test rather than by a comment.
    #[test]
    fn tier0_fits_the_host_budget() {
        assert!(
            TIER0.len() <= TIER0_MAX_BYTES,
            "TIER0 is {} bytes, over the {TIER0_MAX_BYTES} byte cap",
            TIER0.len()
        );
    }

    #[test]
    fn tier0_keeps_the_load_bearing_rules() {
        for phrase in [
            "playbook_catalog",
            // Without a chit-chat guard the agent burns a catalog call on
            // every conversational turn, which is what made the rule
            // load-bearing in the first place.
            "Skip chit-chat.",
            "you MUST offer once to save it with playbook_capture",
            "suppressed_suggestions",
            "by synopsis meaning, not slug",
            "suggestion_dismiss",
            "kind soft",
            "kind hard",
            "review_decide",
            "profile_list",
            "projects_list",
            "playbook_interview",
            "offer a short interview",
        ] {
            assert!(TIER0.contains(phrase), "TIER0 lost the phrase `{phrase}`");
        }
    }

    #[test]
    fn tier0_follows_the_prose_conventions() {
        assert!(
            !TIER0.contains('\u{2014}'),
            "no em-dashes in user-facing text"
        );
        assert!(
            !TIER0.contains('!'),
            "no exclamation marks in user-facing text"
        );
    }
}
