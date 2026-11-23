# Your first run

Three ways to see apb work, from the shortest to the most hands-on. Install apb first: [INSTALL.md](INSTALL.md).

## 1. The demo: no agent, no key

`apb demo --run` installs a
small demo project under the config dir and starts it: a script writes
`report.md`, then the run waits for you at a review (`apb review <run>
review --decision approve`). It needs no agent and no key. `apb demo
--remove` deletes it.

Open `apb dashboard` while it waits to see the run as a graph, with the finished step, the pending review and the two choices.

## 2. A shipped playbook

`apb setup plan` shows what setup would install on this machine and writes nothing; `apb setup apply --plan <digest>` installs the default `apb-*` playbooks, their process skills and role profiles for the agents you have, and registers the MCP server in your agent. `apb setup remove` undoes it. A coding agent can do this for you by following [AGENT-SETUP.md](AGENT-SETUP.md).

Then run one in a git repository of yours:

```sh
apb run apb-code-review --scope global --param target=main..HEAD
apb runs            # the run, its steps and its result
```

Every shipped playbook carries scenarios you can play in seconds without an agent: `apb eval apb-code-review --scope global --level quick`.

## 3. A playbook of your own

The usual way is to ask your coding agent to write one over MCP, or to build it in the dashboard. By hand it looks like this.

A playbook that plans a task with an agent, runs a lint script, and lets the
agent fix the result until the lint passes (at most three times).

1. Set up a project and a profile. A profile says which agent and model run
   a node; `apb detect` lists the agents and models apb found on this machine.

   ```sh
   cd my-project            # any git repository
   apb init                 # creates .apb/
   apb profile write architect --agent claude --model claude-opus-5-5
   ```

2. Save the playbook as `.apb/playbooks/implement-task/1.0.0/playbook.yaml`:

   ```yaml
   schema: 2
   id: implement-task
   name: Implement Task
   version: 1.0.0

   params:
     - { name: task, type: text, label: "Task" }

   defaults:
     profile: architect

   nodes:
     - { id: start, type: start, title: Start }
     - id: plan
       type: agent_task
       title: Plan
       expected_duration: 3m
       prompt: |
         Write a plan: {{params.task}}
     - { id: lint, type: script, title: Lint, expected_duration: 10s, script: scripts/lint.sh, runner: sh }
     - { id: check, type: condition, title: Passed?, max_loops: 3 }
     - id: fix
       type: agent_task
       title: Fix
       expected_duration: 5m
       prompt: |
         Fix: {{nodes.lint.output}}
     - { id: done, type: finish, outcome: success }

   edges:
     - { from: start, to: plan }
     - { from: plan, to: lint }
     - { from: lint, to: check }
     - { from: check, to: done, condition: { type: node_status, node: lint, equals: success } }
     - { from: check, to: fix, condition: { type: node_status, node: lint, equals: failure } }
     - { from: fix, to: lint }
   ```

3. Add the lint script next to it, as
   `.apb/playbooks/implement-task/1.0.0/scripts/lint.sh`, and mark `1.0.0` as
   the current version:

   ```sh
   #!/bin/sh
   # Replace with your real linter; a non-zero exit sends the run to "fix".
   set -e
   test -f README.md || { echo "README.md is missing"; exit 1; }
   echo "lint ok"
   ```

   ```sh
   printf '1.0.0\n' > .apb/playbooks/implement-task/current
   ```

4. Check it and run it:

   ```sh
   apb validate implement-task
   apb run implement-task --param task="add a health endpoint"
   apb dashboard            # watch runs and edit playbooks at http://127.0.0.1:7321
   ```

The dashboard can also create playbooks from scratch, and a coding agent can
write them for you over MCP ([MCP.md](MCP.md)). The full playbook format, node types
and templating are in [docs/HOWTO-authoring.md](HOWTO-authoring.md).
