## What and why

<!-- Which task (e.g. "01 foundation, task 5") or issue ("Fixes #123") this
completes, and what changes. One task or one issue per pull request. -->

## How it was verified

<!-- Commands run and what you checked by hand. Include before/after output for
fixes. -->

## Checklist

- [ ] All quality gates pass locally ([docs/ci.md](https://github.com/BharathChinthallapalli/Test-Waters/blob/main/docs/ci.md))
- [ ] Official docs read for any new crate, API or protocol detail; URLs cited
- [ ] New dependencies: licence compatible with MIT, `cargo deny check` / `pnpm audit` clean
- [ ] No secrets, keys or private data in code, tests or logs
- [ ] No code copied from LGPL/GPL projects
- [ ] `tasks.md` and `docs/progress.md` updated if this completes a spec task
- [ ] New or changed decisions recorded as an ADR
