# APB Operator pack instructions

This pack connects Buzz personas to one local apb (agentic-playbooks) project over MCP.

- The apb MCP server serves exactly one project: the directory named by APB_PROJECT_DIR in this pack's .mcp.json. All playbook, profile, and run tools operate on that project. Reads across other registered workspaces are possible through projects_list plus the workspace parameter, but runs belong to the pinned project.
- Trust is established outside this pack. When apb refuses a draft or untrusted playbook, that refusal is correct behavior; report it and stop.
- Runs survive the conversation. Run state lives in the project's runs/ directory; an interrupted run can be resumed with run_resume once the interruption is resolved.
- Never place secret values in channel messages, run instructions, or tool arguments.
