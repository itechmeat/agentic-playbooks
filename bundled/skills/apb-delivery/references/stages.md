# Stage contracts

Direct work runs these stages in order. An apb playbook runs one stage per
node; the graph owns the order, the checks, the repair bound and the resume.
Do not run the whole delivery flow inside one node.

| Stage | Required result |
| --- | --- |
| Intake | Reference to the original request; the expected-result card; the project's checks, risk triggers and merge policy found |
| Design | One design pass with real advisor evidence and its fallback rung, or a sound reuse of a current design |
| Decompose | The triggers checked; no subtasks, or subtasks with their card outcomes, checks and dependencies in order |
| Implement | A bounded end-to-end change against the card; causal or preservation evidence for the change kind; native check results |
| Review | Independent read-only roles against the card and the actual candidate; findings with consequences, coverage and provenance |
| Repair | Verify and dispose findings, fix the confirmed ones, rerun the affected checks; never restart the whole flow |
| Reconsider | After two repair rounds or repeated regressions, revise the approach with evidence; never lower the acceptance |
| Integrate | Fresh required checks and review; push and pull request only within the authority the user or the project grants; merge and tags per project policy and apb consent |
| Report | Outcome by outcome against the original request; conformance; assumptions; evidence; findings; measurements |

With subtasks, each subtask is implemented, checked and reviewed in the
declared order, with one self-check repair round per subtask. The full
review panel then reviews the integrated result with at most two repair
rounds. The Report checks the whole
original card, not only the sum of the subtasks.

The report keeps three states apart: implementation complete, ready to
integrate, and integrated.

A journal shows that a step ran. Whether it ran well needs its evidence. A
checkbox or an agent's exit does not prove a stage was done well.
