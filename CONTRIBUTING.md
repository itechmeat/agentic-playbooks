# Contributing to Agentic Playbooks

Thank you for helping improve `apb`.

## License of contributions

The project is licensed under the Apache License, Version 2.0.
Unless you explicitly state otherwise, every contribution intentionally
submitted for inclusion in this project is licensed under the same terms.
No separate Contributor License Agreement is required at this time.

## Developer Certificate of Origin

Every commit must include a `Signed-off-by` line certifying the Developer
Certificate of Origin in [DCO](DCO).

Create a signed-off commit with:

```sh
git commit -s -m "Describe the change"
```

If you forgot the sign-off on the latest commit:

```sh
git commit --amend --signoff --no-edit
```

Use your real name and an email address you are entitled to use. The sign-off
means that you wrote the contribution, or otherwise have the right to submit it
under the project's Apache-2.0 license. It is not a copyright assignment.

## Before opening a pull request

- Keep each pull request focused and explain why the change is needed.
- Add or update tests and documentation when behavior changes.
- Do not commit secrets, credentials, private prompts, personal data, or run logs.
- Do not submit third-party code, assets, examples, or generated files unless
  their source and license are clearly identified and compatible with this project.
- If an employer or client may own your work, obtain permission before submitting it.
- If AI tools materially assisted the contribution, disclose that in the pull
  request. You remain responsible for reviewing, testing, and having the right to
  submit all resulting material.

## Security-sensitive changes

Playbooks can execute scripts and invoke coding agents. Treat changes involving
command execution, file access, web or MCP interfaces, credentials, supervisor
permissions, and run-state storage as security-sensitive. Describe the trust
boundary and expected failure modes in the pull request.

## Pull request checklist

- All commits are signed off under the DCO.
- Tests pass locally.
- User-facing changes are documented.
- No secrets or private run data are included.
- Third-party material and licenses are disclosed.
- Security implications have been considered.
