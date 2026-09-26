# 0009. Generate TypeScript types from Rust with ts-rs

- Status: Accepted
- Date: 2026-09-26

## Context

Requirement 3 of feature 01 says TypeScript types are generated from Rust, never
hand-mirrored, and CI fails when the committed types differ from a fresh
generation. The owner listed two candidates in `docs/DECISIONS.md`:

1. **ts-rs:** derive `TS` on Rust types and write TypeScript directly.
2. **schemars → JSON Schema → TypeScript:** derive `JsonSchema`, then convert
   the schema with the Node tool json-schema-to-typescript.

Both read serde attributes, both are MIT-licensed, and both keep Rust as the
single source. To compare output rather than claims, the same sample types (a
camelCase struct with a `u64`, an `Option<String>`, a `(u32, u32)` tuple and an
internally tagged enum) were generated with ts-rs 12.0.1 and with schemars
1.2.2 plus json-schema-to-typescript 16.0.0:

| Rust | ts-rs | schemars (default 2020-12) → TS | schemars (draft-07) → TS |
|---|---|---|---|
| `(u32, u32)` | `[number, number]` | `[unknown, unknown]` | `[number, number]` |
| `Option<String>` | `lastError: string \| null` | `lastError?: string \| null` | same as 2020-12 |
| struct without `deny_unknown_fields` | exact object type | object plus `[k: string]: unknown` | same as 2020-12 |
| tagged enum, camelCase, doc comments | correct | correct | correct |

json-schema-to-typescript models schemas as draft 4 and ignores draft 2020-12's
`prefixItems`, which is what schemars emits by default. The schemars route can
match ts-rs only after three settings (draft-07 output, the serialize contract,
and no additional properties), each of which is a way for the two sides to drift
silently.

## Decision

- Use **ts-rs** to generate TypeScript from Rust wire types.
- ts-rs is a **dev-dependency** of `cs-core`. Types derive it with
  `#[cfg_attr(test, derive(ts_rs::TS), ts(export))]`, so release builds do not
  link it.
- `.cargo/config.toml` sets `TS_RS_EXPORT_DIR` to
  `packages/api-types/src/generated`, `TS_RS_IMPORT_EXTENSION` to `ts` (the
  repository's `nodenext` + `rewriteRelativeImportExtensions` setup) and
  `TS_RS_LARGE_INT` to `number`, because JSON carries integers as numbers, not
  `BigInt`.
- `pnpm gen-types` runs the export tests into an empty scratch folder and
  replaces the generated folder only if they succeed.
  `pnpm check-types` does the same and fails if the result differs from what is
  in git; CI runs it (feature 01, task 5).
- All wire types live in `cs-core`, the crate for domain types. Because the
  derive exists only in `cs-core`'s test builds, another crate cannot generate
  a type that contains a `cs-core` type, and `gen-types` only runs `cs-core`'s
  export tests. A second exporting crate would need a shared `ts` cargo
  feature instead of `cfg_attr(test, …)`; revisit this ADR if one is needed.
- Generated files are never edited by hand and are excluded from Biome
  formatting and linting. `packages/api-types/src/index.ts` re-exports them.

## Consequences

- Rust and TypeScript types cannot drift without CI failing.
- Integers above 2^53 lose precision in JavaScript. Any wire field that can
  exceed that (for example a byte counter) must be sent as a string; this
  applies to any JSON transport, not only to ts-rs.
- ts-rs produces TypeScript only. Where a JSON Schema is itself needed (policy
  file validation in feature 05, the Python replay harness in feature 15),
  schemars can be added to those types; its derive coexists with ts-rs on the
  same struct.
- ts-rs's last tagged release was January 2026. Watch for maintenance, and
  revisit this ADR if it falls behind the Rust or serde versions in use.
- `cargo test` rewrites the bindings as a side effect, so a developer who
  changes a wire type sees the regenerated file in `git status`.

## Sources

- ts-rs README and crate docs at tag `v12.0.0`
  (https://github.com/Aleph-Alpha/ts-rs/blob/v12.0.0/README.md): configuration
  variables, `#[ts(export)]` export tests, serde compatibility. Version 12.0.1
  confirmed from the crates.io index.
- schemars README and `docs/3-generating.md` at tag `v1.2.2`
  (https://github.com/GREsau/schemars/blob/v1.2.2/README.md): draft 2020-12
  default, `SchemaSettings::draft07()`, serde compatibility.
- json-schema-to-typescript README at commit `5caacfc` (release 16.0.0)
  (https://github.com/bcherny/json-schema-to-typescript/blob/5caacfc53671/README.md):
  "JSON Schema draft support" section (draft 4 model; `prefixItems` ignored).
- Biome `files.includes` negated patterns:
  https://biomejs.dev/reference/configuration/#filesincludes (read via the
  Context7 index of github.com/biomejs/website).
- Comparison run: the sample above, generated in a scratch crate on 2026-09-26.
