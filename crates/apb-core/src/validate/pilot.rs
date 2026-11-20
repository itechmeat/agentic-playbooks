//! The `list.pilot` and `list.stop_when` checks: a slice within the
//! list's bounds (V240), a stop rule that names one (V241), and a
//! `then: continue` without an item check (V242).

use crate::list::PilotThen;
use crate::schema::Playbook;

use super::*;

pub(super) fn check_pilot(playbook: &Playbook, r: &mut ValidationReport) {
    for node in &playbook.nodes {
        let Some(list) = node.list() else {
            continue;
        };
        if let Some(pilot) = &list.pilot {
            if pilot.items == 0 || pilot.items > crate::list::PILOT_MAX_ITEMS {
                r.error(
                    "V240",
                    Some(&node.id),
                    format!(
                        "list.pilot.items is {}; use 1 to {}",
                        pilot.items,
                        crate::list::PILOT_MAX_ITEMS
                    ),
                );
            } else if pilot.items >= list.items.max_items {
                r.error(
                    "V240",
                    Some(&node.id),
                    format!(
                        "list.pilot.items {} is not below list.items.max_items {}: the pilot would be the whole list",
                        pilot.items, list.items.max_items
                    ),
                );
            } else if list.items.from.is_none() && (list.items.inline.len() as u32) <= pilot.items {
                r.error(
                    "V240",
                    Some(&node.id),
                    format!(
                        "the list has {} inline items; a pilot of {} runs them all",
                        list.items.inline.len(),
                        pilot.items
                    ),
                );
            }
            if pilot.then == PilotThen::Continue && list.check.is_none() {
                r.warn(
                    "V242",
                    Some(&node.id),
                    "list.pilot.then: continue goes on by itself when every pilot item passes, and without an item check every item passes; add a check or use then: review"
                        .to_string(),
                );
            }
        }
        if let Some(stop) = &list.stop_when {
            if stop.needs_look_at_least.is_none() && stop.needs_look_share.is_none() {
                r.error(
                    "V241",
                    Some(&node.id),
                    "list.stop_when sets no rule; give needs_look_at_least or needs_look_share"
                        .to_string(),
                );
            }
            if stop.needs_look_at_least == Some(0) {
                r.error(
                    "V241",
                    Some(&node.id),
                    "list.stop_when.needs_look_at_least is 0; use 1 or more".to_string(),
                );
            }
            match stop.needs_look_share {
                Some(s) if !s.is_finite() || s <= 0.0 || s > 1.0 => {
                    r.error(
                        "V241",
                        Some(&node.id),
                        format!(
                            "list.stop_when.needs_look_share is {s}; use a share above 0 and at most 1"
                        ),
                    );
                }
                None if stop.min_settled.is_some() => {
                    r.error(
                        "V241",
                        Some(&node.id),
                        "list.stop_when.min_settled needs needs_look_share".to_string(),
                    );
                }
                _ => {}
            }
            if stop.min_settled == Some(0) {
                r.error(
                    "V241",
                    Some(&node.id),
                    "list.stop_when.min_settled is 0; use 1 or more".to_string(),
                );
            }
        }
    }
}
