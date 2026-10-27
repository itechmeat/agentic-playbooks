//! Loads a list node's items: from an earlier node's declared output
//! field, from a list node's own items, or inline, then the `where` filter.
//! A source that cannot be loaded fails the node before any item runs.

use apb_core::list::ListSpec;
use apb_core::list::items::{
    Item, SourcedItem, apply_filter, items_from_list_output, items_from_output, items_from_values,
};
use apb_core::schema::Playbook;

use crate::state::RunState;

/// A visit's items after the filter.
pub(super) struct Loaded {
    pub items: Vec<Item>,
    pub filtered_out: u32,
}

/// The items of `spec` against the run's state, or why they cannot be
/// loaded (duplicate ids, too many, a source that is not a list).
pub(super) fn load(
    playbook: &Playbook,
    state: &RunState,
    spec: &ListSpec,
) -> Result<Loaded, String> {
    let src = &spec.items;
    let sourced: Vec<SourcedItem> = match &src.from {
        None => plain(items_from_values(&src.inline, &src.key, src.max_items))?,
        Some(from) => {
            let output = state
                .outputs
                .get(&from.node)
                .ok_or_else(|| format!("source `{}` has no output", from.node))?;
            if playbook
                .node(&from.node)
                .is_some_and(|n| n.list().is_some())
            {
                items_from_list_output(output, src.max_items).map_err(|e| e.to_string())?
            } else {
                plain(items_from_output(
                    output,
                    &from.field,
                    &src.key,
                    src.max_items,
                ))?
            }
        }
    };
    let (items, filtered_out) = apply_filter(sourced, src.filter.as_ref());
    Ok(Loaded {
        items,
        filtered_out,
    })
}

fn plain(
    items: Result<Vec<Item>, apb_core::list::items::ItemsError>,
) -> Result<Vec<SourcedItem>, String> {
    Ok(items
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|item| SourcedItem { item, state: None })
        .collect())
}
