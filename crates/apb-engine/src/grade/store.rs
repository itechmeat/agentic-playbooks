//! Stored grades: `runs/<id>/grades/<grade_id>.json`, written once
//! (atomically, owner-only) and never changed. A run may have several
//! grades; a comparison reads the latest of each grader.

use std::path::{Path, PathBuf};

use apb_core::quality::grade::{Grade, GraderId};

use crate::error::EngineError;

pub const GRADES_DIR: &str = "grades";

/// A new grade id: the grader, the time and a random suffix.
pub fn new_grade_id(grader: GraderId, created_at: u64) -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!(
        "{}-{created_at}-{}",
        grader.to_string().replace('/', "-"),
        &suffix[..8]
    )
}

fn path_of(run_dir: &Path, grade_id: &str) -> Result<PathBuf, EngineError> {
    if !apb_core::registry::is_safe_segment(grade_id) {
        return Err(EngineError::Invalid(format!("grade id `{grade_id}`")));
    }
    Ok(run_dir.join(GRADES_DIR).join(format!("{grade_id}.json")))
}

/// Writes `grade` into its run. A grade id that is already taken is
/// refused: grades are write-once.
pub fn write(run_dir: &Path, grade: &Grade) -> Result<(), EngineError> {
    let path = path_of(run_dir, &grade.grade_id)?;
    if path.exists() {
        return Err(EngineError::Conflict(format!(
            "grade `{}` already exists",
            grade.grade_id
        )));
    }
    apb_core::fsutil::create_dir_under(run_dir, &run_dir.join(GRADES_DIR))?;
    let json = serde_json::to_vec_pretty(grade).map_err(|e| EngineError::Invalid(e.to_string()))?;
    apb_core::fsutil::atomic_write_private(&path, &json)?;
    Ok(())
}

/// Every readable grade of the run at `run_dir`, oldest first. A file that
/// does not read (a newer apb's format, a torn copy) is left out.
pub fn grades_of(run_dir: &Path) -> Vec<Grade> {
    let Ok(entries) = std::fs::read_dir(run_dir.join(GRADES_DIR)) else {
        return Vec::new();
    };
    let mut out: Vec<Grade> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|raw| serde_json::from_slice::<Grade>(&raw).ok())
        .collect();
    out.sort_by(|a, b| (a.created_at, &a.grade_id).cmp(&(b.created_at, &b.grade_id)));
    out
}

/// The latest grade of `grader` for the run, if any.
pub fn latest_of(run_dir: &Path, grader: GraderId) -> Option<Grade> {
    grades_of(run_dir)
        .into_iter()
        .rev()
        .find(|g| g.grader == grader)
}
