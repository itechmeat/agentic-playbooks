//! Pins each publish node's target before the first acting node runs: the
//! remote's push URL and the repository's common directory, read with the
//! hardened, pinned git of the run's tree. Once an agent or a script ran,
//! the tree's config is theirs, so nothing is pinned any more: a publish
//! node added later (a supervisor patch) has no pin, and its plan refuses
//! `no_pin`. The pin reads the tree the plan will read, the source node's
//! (its `workdir`, else the run's tree); a source whose `workdir` cannot be
//! resolved yet records `no_pin`.

use std::path::{Path, PathBuf};

use apb_core::git::Repo;
use apb_core::publish::Refusal;
use apb_core::publish::url::PushUrl;
use apb_core::schema::{NodeKind, Playbook};

use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload};

/// The push URL of `remote` in the repository `repo`, as the tree's own
/// config names it (`remote get-url --push`).
pub fn push_url(repo: &Repo, dir: &Path, remote: &str) -> Option<String> {
    if !apb_core::publish::refname::remote_ok(remote) {
        return None;
    }
    repo.read(dir, &["remote", "get-url", "--push", remote])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// What the pin of a remote reads: the URL and the scheme, or the refusal.
fn read_pin(tree: Option<&Path>, remote: &str) -> (Option<PushUrl>, String, Option<Refusal>) {
    let Some(tree) = tree else {
        return (None, String::new(), Some(Refusal::NoPin));
    };
    let Ok(repo) = Repo::locate(tree) else {
        return (None, String::new(), Some(Refusal::NotARepository));
    };
    let common = repo.common_dir.to_string_lossy().into_owned();
    match push_url(&repo, tree, remote) {
        None => (None, common, Some(Refusal::RemoteMissing)),
        Some(text) => match PushUrl::parse(&text) {
            Ok(url) => (Some(url), common, None),
            // A refused URL is never journaled: it may carry a credential.
            Err(_) => (None, common, Some(Refusal::UrlRefused)),
        },
    }
}

/// Whether a node that can act, other than `except`, has started in the run
/// `events` belong to.
pub fn acted(playbook: &Playbook, events: &[Event], except: Option<&str>) -> bool {
    events.iter().any(|e| match &e.payload {
        EventPayload::NodeStarted { node, .. } if Some(node.as_str()) != except => {
            playbook.node(node).is_none_or(apb_core::effects::node_acts)
        }
        _ => false,
    })
}

/// Journals a pin for every publish node that has none, when no acting
/// node has started yet, in this run or (`parent_acted`) in a parent run
/// before it started this one. `tree_of` resolves a source node's tree,
/// `None` when its `workdir` cannot be resolved yet. Idempotent.
pub fn ensure(
    playbook: &Playbook,
    events: &[Event],
    log: &mut EventLog,
    parent_acted: bool,
    tree_of: impl Fn(&str) -> Option<PathBuf>,
) -> Result<(), EngineError> {
    let nodes: Vec<(&str, &str, &str)> = playbook
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Publish { from, remote, .. } => {
                Some((n.id.as_str(), from.as_str(), remote.as_str()))
            }
            _ => None,
        })
        .filter(|(id, _, _)| super::pin_of(events, id).is_none())
        .collect();
    if nodes.is_empty() || parent_acted || acted(playbook, events, None) {
        return Ok(());
    }
    for (node, from, remote) in nodes {
        let (url, common_dir, refusal) = read_pin(tree_of(from).as_deref(), remote);
        log.append(EventPayload::PublishTargetPinned {
            node: node.to_string(),
            remote: remote.to_string(),
            scheme: url.as_ref().map(|u| u.scheme().as_str().to_string()),
            url: url.map(|u| u.as_str().to_string()),
            common_dir,
            refusal: refusal.map(|r| r.code().to_string()),
        })?;
    }
    Ok(())
}
