//! The writing brief: how apb asks an agent to write text a person reads.
//! It is added only where a person reads the output, so every other prompt
//! (and its cache key) stays exactly as it was:
//!
//! - the finish composer's closing answer, unless the finish node says
//!   `audience: agents`;
//! - an `agent_task` marked `audience: people` (a PR description, a mail
//!   draft, a note in a CRM);
//! - a node granted a connector function with a `people:` block. Its
//!   connector block also names the args people read, one line per
//!   function (`crate::connector::prompt`).

use std::collections::BTreeMap;

use apb_core::connector::ConnectorDoc;
use apb_core::schema::NodeKind;

use crate::manifest::ManifestConnectorGrant;

/// The brief, a prompt section of its own.
pub const WRITING_BRIEF: &str = concat!(
    "## writing for people\n\n",
    "Part of what you write is read by a person, not by the next step of this run. ",
    "For that text:\n\n",
    "- Lead with the conclusion: the first sentence says what happened, or what is needed.\n",
    "- Say who acts next and what they do, whenever someone has to act.\n",
    "- State facts: numbers, names, links. Mark what you did not check yourself as not verified.\n",
    "- Use the reader's words. Leave out apb's internal terms (node ids, attempts, run context) ",
    "and file paths unless the reader needs them to act.\n",
    "- Keep it short: cut whatever does not help the reader decide or act.\n",
);

/// Whether a node's prompt carries the brief: the node writes for people
/// ([`NodeKind::writes_for_people`]), or one of the functions its `grants`
/// allow (described by `docs`) declares a `people:` block.
pub fn applies(
    node: &NodeKind,
    grants: &[ManifestConnectorGrant],
    docs: &BTreeMap<String, ConnectorDoc>,
) -> bool {
    node.writes_for_people()
        || crate::connector::prompt::granted_functions(grants, docs).any(|f| f.people.is_some())
}

/// `text` with the brief appended as its own section.
pub fn append_to(text: &mut String) {
    text.push_str("\n\n");
    text.push_str(WRITING_BRIEF);
}
