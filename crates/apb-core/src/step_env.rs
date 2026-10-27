//! Whether this process runs as a step of a run. The engine sets
//! `APB_RUN_ID` for every agent and script step and `APB_HOST_TASK` for a
//! host task's subagent; a person's terminal sets neither. The CLI and the
//! MCP server run in their caller's process, so this is how they tell a
//! step apart from a person for the decisions only a person may make. It
//! guards against a step doing so by mistake, not against an attacker who
//! clears the variables.

/// The run step this process was started from, if any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepCaller {
    pub run_id: Option<String>,
    pub host_task: Option<String>,
}

impl StepCaller {
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        StepCaller {
            run_id: var("APB_RUN_ID"),
            host_task: var("APB_HOST_TASK"),
        }
    }

    /// Where the step runs, for a refusal; `None` for a person.
    pub fn describe(&self) -> Option<String> {
        match (&self.run_id, &self.host_task) {
            (Some(run), _) => Some(format!("inside run `{run}` (APB_RUN_ID is set)")),
            (None, Some(task)) => Some(format!("inside host task `{task}` (APB_HOST_TASK is set)")),
            (None, None) => None,
        }
    }

    /// The refusal of `what` when this process is a run step: a step does
    /// not judge or decide a playbook's runs (grading, promoting); `None`
    /// outside a run.
    pub fn refusal(&self, what: &str) -> Option<String> {
        self.describe().map(|from| {
            format!(
                "{what} is refused {from}: a run step does not grade runs or decide a playbook's candidate; do it outside a run (the CLI, the dashboard, or an agent session that is not a run step)"
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_refused_and_a_person_is_not() {
        assert_eq!(StepCaller::default().refusal("grading"), None);
        let step = StepCaller {
            run_id: Some("r1".into()),
            host_task: None,
        };
        let why = step.refusal("grading").unwrap();
        assert!(why.starts_with("grading is refused inside run"), "{why}");
        assert!(why.contains("run `r1`"), "{why}");
    }
}
