# APB Operator pack for Buzz

A single-persona Buzz pack that turns a channel into an apb control room: ask the agent to list playbooks, approve a run, watch progress, and answer human-review gates without leaving the chat.

Setup, project pinning, and troubleshooting live in the recipe: [docs/integrations/buzz.md](../../../docs/integrations/buzz.md).

Quick check after editing the pack:

    buzz pack validate examples/buzz/apb-operator-pack
    buzz pack inspect examples/buzz/apb-operator-pack

Both commands are local-only and need no relay.
