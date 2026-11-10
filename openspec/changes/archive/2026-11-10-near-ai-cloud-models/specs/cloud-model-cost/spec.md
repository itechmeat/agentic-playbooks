## ADDED Requirements

### Requirement: Attempts on catalog models are priced from the catalog
When an agent attempt on a cloud catalog model reports tokens and no cost, apb SHALL compute `cost_usd` from the run's snapshot of the row's prices (input, cached input or else input, output) and SHALL record `priced_from` with the catalog, model, digest and fetch time. A cost the agent printed SHALL never be replaced, and a row without prices SHALL leave the cost absent.

#### Scenario: A dsh step on NEAR
- **WHEN** a step on `nearai/Qwen/Qwen3.8-27B` reports 1,000,000 input tokens and 100,000 output tokens
- **THEN** `attempt_finished.usage.cost_usd` is 0.175 and `priced_from.catalog` is `near-ai-cloud`

#### Scenario: An older journal
- **WHEN** an apb without this change reads a journal with `priced_from`
- **THEN** the attempt's usage still decodes

### Requirement: The basis of a keyed cloud route is billed
The cost basis of an attempt on a cloud route SHALL be `billed` when the route's key variable reaches the spawned process, before the agent module's own reading; without the key the cloud step SHALL decide nothing. The run gate SHALL predict the same basis, so the `billing:` section's `billed` policy decides the start.

#### Scenario: Billing ask
- **WHEN** `billing.billed` is `ask` and a person runs a playbook on `nearai/Qwen/Qwen3.8-27B` without confirming
- **THEN** the start is refused with the launch nonce for billed work naming dsh

### Requirement: Budgets, estimates and stats count the priced cost
The run budget's spent amount, the run estimate's history samples, `apb stats` and every run surface SHALL count a priced cost exactly like a printed one, and the run surfaces SHALL say that it was priced from the catalog.

#### Scenario: A budget stop
- **WHEN** a run with `budget: { usd: 0.10 }` has spent 0.09 on NEAR steps and the next node's median is 0.05
- **THEN** the budget gate pauses the run before that node
