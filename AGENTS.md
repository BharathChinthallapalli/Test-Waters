# Instructions for coding agents

Callsheet (working name) is a local-first desktop control plane for AI coding
workers. Rust daemon, proxy and store; Electron + TypeScript desktop app.

## Read first

- `.kiro/steering/product.md`, `tech.md`, `structure.md`, `workflow.md`:
  always-applicable rules. They win over anything else in this file.
- `docs/ROADMAP.md`: feature order. The active spec is the first unfinished
  feature under `.kiro/specs/`.
- `docs/adr/`: accepted decisions. Don't reopen them; write a new ADR instead.
- `docs/progress.md`: learnings from earlier sessions; append yours.

## How to work

1. Take the first unticked task in the active spec's `tasks.md`. One task per
   session; anything extra goes to `docs/progress.md` as a follow-up.
2. Read the official docs before using a new crate, API or protocol detail, and
   cite the URLs in the commit message or ADR.
3. Blocked on an owner decision (name, licence, scope)? Write `BLOCKED:` under
   the task and stop. Never guess.

## Quality gates (all must pass; see docs/ci.md)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
pnpm biome check .
pnpm -r typecheck
pnpm -r test
pnpm check-types
```

Never weaken a lint, test or gate to make it pass.

## Non-negotiable rules

- Never log, store or echo API keys or auth headers.
- Private keys live only in the daemon; workers never hold keys.
- TypeScript types are generated from Rust (`pnpm gen-types`); never edit
  `packages/api-types/src/generated` by hand.
- Content capture is off by default; nothing leaves the machine by default
  (PRIVACY.md).
- Don't copy code from LGPL-licensed projects (for example PI-Desktop); ideas
  only. The project is MIT.
- Never run agents with permission checks disabled.
