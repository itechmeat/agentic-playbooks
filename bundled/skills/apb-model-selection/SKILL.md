---
name: apb-model-selection
description: Choose the agent and model for each role of a task (decide, build, review) from what this user actually has installed and pays for, with a fallback chain. Use when writing or changing a profile, when setup plans role profiles, when a step must be delegated, and when an executor hits a limit.
metadata:
  version: "1.0.0"
---

# Choose executors and models

Pick from what exists on this machine and what the user pays for, never from
memory. Names change; the rules below do not name any model or provider.

## 1. Find out what is available

- **Agents:** `apb detect --json` (MCP `agents_detect`): which agent CLIs are
  installed, their versions, and `options_by_agent`, the model ids apb offers
  for each agent.
- **Subscriptions:** `apb subscriptions`: which agents the user pays for, on
  which plan. A subscribed agent comes before a pay-per-use one of the same
  strength.
- **The models table apb ships:** each model's vendor, price, reasoning
  level and context size, and the `purposes` that rank models for kinds of
  work (coding, review, planning, cheap glue and others).
  `options_by_agent` is built from it, the same lists the dashboard shows.
- **The user's own guidance:** project or user instructions that name
  executors win over everything here.

An agent counts only when it is installed and has a model apb can launch.

## 2. Classes of role

| Role | Needs | Pick |
|------|-------|------|
| Decide or classify | short answers, many calls, low cost | the host's small fast model, or the cheapest model rated for cheap glue |
| Design and build | deep reasoning, long context, code quality | the strongest model rated for coding or planning that the user has |
| Review | independence from the builder | a strong model of another family than the builder's; else another agent; else the same agent in a fresh session |
| Research | reading many sources, citing them | a model with long context rated for research |

"Family" means the vendor of the model. Two agents that run the same
vendor's models are one family.

## 3. Fallback chain

Every profile names fallbacks in order:

1. another available family of similar strength;
2. another agent of the same family;
3. a fresh, clean-context session of the same executor that gets only the
   task packet.

Record which rung actually ran. A configured model is a request, not proof.

## 4. Limits and balance are routing signals

A usage limit, a rate limit, a quota pause or a zero balance moves the work
to the next rung. It is never a reason to stop the task or to wait without a
bound. When every rung is exhausted, say so and name the earliest time one
returns.

## 5. Cross-family review is required

When the user has any model of another family than the builder's, a review
round by that family is required before a substantive change is integrated.
Only when no other family exists does a fresh same-family session stand in,
and the report says the review was same-family.

## 6. Write it down

In a profile: `executor.agent`, `executor.model` and `executor.fallbacks`
(each fallback an `{ agent, model }` entry) in the order above; the third
rung, a fresh session, needs no entry. In a report: the role, the executor
that ran, its rung and why it was chosen, and its cost line: the figure or
"no figure", its basis (billed, subscription equivalent and not billed, or
basis unknown) and the class of login it loaded (an api key, a subscription
login, a cloud account, or unknown), never a secret value. Do not hardcode a model in a
shared playbook; bind it through a profile.

The role profiles `apb setup` writes rank the other usable agents by the
family of the model each runs: another known family first, an unknown
family next, the profile's own family (and, for the reviewer, the
builder's) last, in subscription order, then detection order, within each.
An agent whose model's family apb cannot tell ranks as unknown; check
those against section 3.
