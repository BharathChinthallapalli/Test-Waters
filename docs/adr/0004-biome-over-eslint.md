# 0004. Use Biome, not ESLint and Prettier, for TypeScript and JSON

- Status: Accepted
- Date: 2026-09-26

## Context

The TypeScript side (Electron app, generated types) needs a formatter and a
linter that run locally and in CI with no extra setup. The usual pairing is
ESLint for linting plus Prettier for formatting: two tools, two configs, and a
plugin set to keep in sync.

Biome is one binary that formats and lints JavaScript, TypeScript, JSX, JSON
and CSS from a single `biome.json`. Its linter includes rules taken from
ESLint, TypeScript ESLint and other sources, and `biome migrate eslint` can
port an ESLint configuration if one is ever needed. Custom rules can be
added as GritQL plugins.

## Decision

Biome is the only formatter and linter for TypeScript, JavaScript and JSON.
The version is pinned in the root `package.json` (2.5.14 at the time of
writing) and configured in the root `biome.json` with the recommended rule
preset. The quality gate is `pnpm biome check .`. Type checking stays with
`tsc --noEmit` through `pnpm -r typecheck`.

## Consequences

- One tool and one config to maintain; formatting and lint run in one pass.
- ESLint plugins do not run. If a rule we need exists only as an ESLint
  plugin, we either write it as a Biome GritQL plugin or revisit this ADR.
- Biome reads `.gitignore`, so build output is skipped without a separate
  ignore list.
- Rust keeps its own tools (rustfmt, clippy); Biome does not cover Rust.

## Sources

- Biome configuration reference: https://biomejs.dev/reference/configuration/
- Biome linter and rule sources: https://biomejs.dev/linter/ and
  https://biomejs.dev/linter/rules-sources/
- Migrating from ESLint and Prettier:
  https://biomejs.dev/guides/migrate-eslint-prettier/
- GritQL plugins: https://biomejs.dev/recipes/gritql-plugins/

All read via the Context7 index of github.com/biomejs/website; the default
configuration was produced by `biome init` from @biomejs/biome 2.5.14.
