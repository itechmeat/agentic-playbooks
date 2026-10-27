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
}
