//! Run quality: a playbook's `quality:` block (the criteria a finished run
//! is graded on), the grade a grader writes for a run, and the comparison
//! that promotes a candidate version by quality.
//!
//! - [`criteria`]: the block, the built-in criteria and their digests;
//! - [`grade`]: the stored grade, its levels, citations and reading trace;
//! - [`compare`]: the comparability key and the deterministic verdict.
//!
//! Pure data and rules; the graders and the store live in the engine.

pub mod compare;
pub mod criteria;
pub mod grade;

pub use compare::{ComparableKey, QualityDecision, QualityRecord, Verdict, decide};
pub use criteria::{CriterionDef, CriterionSource, QualityBlock};
pub use grade::{CriterionGrade, Grade, GraderId, Level};
