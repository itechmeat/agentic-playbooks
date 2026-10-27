//! What runs as a run ends, right before its `run_finished` (every path
//! that ends a run goes through [`finish_hooks`]): the inline grade by
//! rules (`run_graded`), then the candidate trial's settlement
//! (`candidate_promoted`, `candidate_rejected` or `candidate_held`). Both
//! are best effort: a fault is reported on stderr and never changes the
//! run's outcome. Every event they write is followed by `run_finished`, a
//! checkpoint, so an older apb skips them safely.

use super::*;

/// Grades the ending run, then settles its candidate trial. `succeeded` is
/// the outcome `run_finished` will carry.
pub(crate) fn finish_hooks(log: &mut EventLog, run_dir: &Path, succeeded: bool) {
    grade(log, run_dir, succeeded);
    crate::candidate::settle(log, run_dir, succeeded);
}

fn grade(log: &mut EventLog, run_dir: &Path, succeeded: bool) {
    let grade = match crate::grade::grade_ending(run_dir, succeeded) {
        Ok(Some(g)) => g,
        Ok(None) => return,
        Err(e) => {
            eprintln!("apb: could not grade the run: {e}");
            return;
        }
    };
    let appended = log.append(EventPayload::RunGraded {
        levels: grade
            .criteria
            .iter()
            .map(|c| (c.id.clone(), c.level.as_str().to_string()))
            .collect(),
        grader: grade.grader.to_string(),
        grade_id: grade.grade_id,
    });
    if let Err(e) = appended {
        eprintln!("apb: could not journal the run's grade: {e}");
    }
}
