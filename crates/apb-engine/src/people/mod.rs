//! Text apb writes for people, and what it asks agents to write for them:
//! the instructions of a gate or a wake ([`instruction`]), the writing
//! brief added to prompts whose output people read ([`brief`]), and the
//! link to a run ([`link`]). Notifications build on these
//! (`crate::notify::notice`).

pub mod brief;
pub mod instruction;
pub mod link;
