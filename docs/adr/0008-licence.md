# 0008. Choose the project licence

- Status: Proposed — owner decision required
- Date: 2026-09-26

## Context

Callsheet is a personal open-source project. The licence is an owner decision
(`.kiro/steering/product.md`: "Licence and final name are owner decisions;
never pick them in a session"). This ADR lists the options so the owner can
decide; it does not choose.

Feature 01 requirement 6.4 says no LICENSE file is added while this ADR is
Proposed. However, the repository already contains an MIT `LICENSE` from its
initial commit, created before the spec existed. That file was left
untouched; resolving the conflict is part of this decision. Tracked in
issue #4.

Options:

1. **MIT.** Short and permissive. Requires keeping the copyright and
   permission notice. Grants no explicit patent licence.
2. **Apache-2.0.** Permissive, with an explicit patent licence from
   contributors (section 3, "Grant of Patent License") that terminates for
   anyone who brings patent litigation over the work, and rules for
   `NOTICE` files and marking changed files (section 4).
3. **Dual `MIT OR Apache-2.0`.** Users pick either licence (for example,
   `@biomejs/biome` declares `MIT OR Apache-2.0`). It needs two licence files and the SPDX expression in
   every manifest.

## Decision

None yet. The owner picks one option and changes this ADR's status to
Accepted.

## Consequences

Once decided:

- Keep or replace the existing `LICENSE` file to match (with Apache-2.0 or
  dual licensing, add `LICENSE-APACHE` and, for dual, `LICENSE-MIT`).
- Set `license` in `[workspace.package]` of the root `Cargo.toml` and in each
  `package.json`, using the SPDX expression (`MIT`, `Apache-2.0`, or
  `MIT OR Apache-2.0`).
- Configure the allowed licence list in `deny.toml` (feature 01, task 5.2) to
  be compatible with the choice.
- State the licence in README and CONTRIBUTING (feature 01, task 6.1).

Until then, feature 01 task 6.3 stays blocked on this ADR.

## Sources

- Apache License 2.0: https://www.apache.org/licenses/LICENSE-2.0.txt
  (sections 3 and 4 as quoted above).
- MIT License text as committed in this repository's `LICENSE`
  (initial commit `eab299e`).
- `@biomejs/biome` 2.5.14 licence field, from
  https://registry.npmjs.org/@biomejs/biome/2.5.14
