---
inclusion: always
---
# Workflow: one feature, one task at a time

1. Only one spec is active: the first unfinished entry in docs/ROADMAP.md.
2. A new spec is written requirements.md -> design.md -> tasks.md, and the owner approves
   each file before the next is written. Never draft specs for later features early.
3. One top-level task per session. Read its `_Requirements_` first. No scope creep:
   anything extra goes to docs/progress.md as a follow-up.
4. Before using a new crate, API or protocol detail, read its official docs; cite URLs in the
   task's commit message or ADR.
5. Run all quality gates before marking a task `[x]`. Never weaken a lint or test to pass.
6. Append learnings to docs/progress.md. Stop after the task.
7. Blocked on an owner decision? Write `BLOCKED:` under the task and stop. Never guess.
8. Never run agents with permission checks disabled; use explicit allowlists.

Quality gates: `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings`
· `cargo test --workspace` · `pnpm biome check .` · `pnpm -r typecheck` · `cargo deny check`
