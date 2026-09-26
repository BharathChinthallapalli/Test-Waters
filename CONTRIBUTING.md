# Contributing

Thanks for helping. Callsheet is a personal open-source project built one
feature at a time; this page explains how changes land.

## How work is organised

- `docs/ROADMAP.md` lists features in build order. Only one feature has an
  active spec at a time, under `.kiro/specs/<feature>/` (requirements → design →
  tasks).
- Each pull request does **one task** from the active spec's `tasks.md`, or
  fixes one issue. Anything else you notice goes into `docs/progress.md` or a
  new issue, not into the same PR.
- Architecture decisions live in `docs/adr/`. To change one, propose a new ADR
  rather than editing an accepted one.
- `.kiro/steering/` holds the rules every change follows. Coding agents also
  read [AGENTS.md](AGENTS.md).

## Making a change

1. Open or pick an issue, or the next task in the active spec.
2. Before using a new crate, API or protocol detail, read its official
   documentation and cite the URL in your commit message or ADR.
3. Run every quality gate locally ([docs/ci.md](docs/ci.md)). Never weaken a
   lint, test or gate to make it pass.
4. Open a pull request using the template. CI must be green (**CI passed**)
   before it is merged. While the repository is private, that rule is enforced
   by the maintainer rather than a branch ruleset (issue #28).

New dependencies need their licence and advisories checked first:
`cargo deny check` for Rust, `pnpm audit` for npm. Licences must be compatible
with MIT.

## Code you may not bring in

- **No copied code from LGPL- or GPL-licensed projects** (for example
  PI-Desktop). Learning from their design is fine; copying code is not.
- No code, data or infrastructure from an employer.
- No secrets, real API keys or personal data in code, tests, fixtures or logs.
  Use synthetic values.

## Licence

Callsheet is licensed under the [MIT licence](LICENSE). By contributing, you
agree that your contribution is licensed under the same terms.

## Conduct and security

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md). Report
security problems privately as described in [SECURITY.md](SECURITY.md), never
in a public issue.
