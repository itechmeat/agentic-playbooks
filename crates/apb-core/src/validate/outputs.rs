//! Run outputs rules (V131-V135) for the playbook-level `outputs:` block:
//! names follow `[a-z][a-z0-9_]*` and at most 64 are declared (V131); every
//! output has a source and every source names a node of the playbook, with
//! at most one of `field` and `item_field` (V132); a `field` the node does
//! not publish is reported like V46 (V133); an `item_field` needs a list
//! node (V134) and, when the list declares `item_outputs`, one of them
//! (V135). An invalid `pick` does not load. A source node no run can
//! reach needs no rule of its own: V07 already refuses it.

use super::*;
use crate::run_outputs::{MAX_NAME_LEN, MAX_OUTPUTS, is_valid_name};

/// Runs every structural run outputs rule.
pub(crate) fn check_outputs(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    if playbook.outputs.is_empty() {
        return;
    }
    if playbook.outputs.len() > MAX_OUTPUTS {
        r.error(
            "V131",
            None,
            format!(
                "outputs declares {} outputs, at most {MAX_OUTPUTS} are allowed",
                playbook.outputs.len()
            ),
        );
    }
    let fields = super::node_io::declared_fields(playbook, ctx);
    for (name, output) in playbook.outputs.iter() {
        if !is_valid_name(name) {
            r.error(
                "V131",
                None,
                format!(
                    "output name `{name}` must match [a-z][a-z0-9_]* and be at most {MAX_NAME_LEN} characters"
                ),
            );
        }
        let sources = output.from.as_slice();
        if sources.is_empty() {
            r.error(
                "V132",
                None,
                format!("output `{name}` has no source in `from`"),
            );
        }
        for source in sources {
            let Some(node) = playbook.node(&source.node) else {
                r.error(
                    "V132",
                    None,
                    format!(
                        "output `{name}` reads node `{}`, which does not exist",
                        source.node
                    ),
                );
                continue;
            };
            let id = Some(node.id.as_str());
            if source.field.is_some() && source.item_field.is_some() {
                r.error(
                    "V132",
                    id,
                    format!("output `{name}`: a source takes `field` or `item_field`, not both"),
                );
            }
            if let Some(field) = &source.field
                && let Some(published) = fields.get(node.id.as_str())
                && !published.iter().any(|f| f == field)
            {
                r.warn(
                    "V133",
                    id,
                    format!(
                        "output `{name}` reads field `{field}` of `{}`, which publishes only [{}]",
                        node.id,
                        published.join(", ")
                    ),
                );
            }
            if let Some(item_field) = &source.item_field {
                match node.list() {
                    None => r.error(
                        "V134",
                        id,
                        format!(
                            "output `{name}` reads item_field `{item_field}` of `{}`, which is not a list node",
                            node.id
                        ),
                    ),
                    Some(list)
                        if !list.item_outputs.is_empty()
                            && !list.item_outputs.iter().any(|f| f.name() == item_field) =>
                    {
                        r.warn(
                            "V135",
                            id,
                            format!(
                                "output `{name}` reads item_field `{item_field}` of `{}`, which keeps only item_outputs [{}]",
                                node.id,
                                list.item_output_names().join(", ")
                            ),
                        )
                    }
                    Some(_) => {}
                }
            }
        }
    }
}
