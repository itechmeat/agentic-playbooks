//! List mode's pure half, shared by the drive and the run surfaces: the
//! journal fold of a visit ([`ledger`]), the per-item prompt section
//! ([`prompt`]), the item stamp on events ([`stamp`]) and the surface view
//! ([`view`]). The runner itself lives in `scheduler::list`.

pub mod ledger;
pub mod prompt;
mod stamp;
pub mod view;

pub use ledger::{ItemRecord, ListCounts, ListFold, progress_label};
pub use view::{ListProgress, RunList, RunListItem, run_lists};
