# Tasks — 01 foundation

- [x] 1. Set up workspaces
  - [x] 1.1 Root Cargo.toml (resolver 3, workspace.package, workspace.lints) and stub crates cs-core, cs-store, cs-proxy, cs-daemon
  - [x] 1.2 rust-toolchain.toml and .gitignore
  - [x] 1.3 pnpm-workspace.yaml, root package.json scripts, biome.json
  - _Requirements: 1.1, 1.2, 1.3, 1.4_

- [x] 2. Record architecture decisions
  - [x] 2.1 docs/adr/template.md and ADRs 0001–0007 with sources
  - [x] 2.2 ADR 0008 licence with status Proposed (owner decision)
  - _Requirements: 5.1, 5.2, 5.3_

- [x] 3. Build the hardened desktop shell
  - [x] 3.1 Window factory with hardened webPreferences plus unit test asserting every flag
  - [x] 3.2 Block navigation and window.open plus tests
  - [x] 3.3 CSP (meta + header) and typed empty preload API
  - _Requirements: 2.1, 2.2, 2.3, 2.4_

- [ ] 4. Generate shared types
  - [ ] 4.1 Research generators from official docs; write the ADR
  - [ ] 4.2 Export one sample type from cs-core into packages/api-types via `gen-types`
  - _Requirements: 3.1, 3.2, 3.3_

- [ ] 5. Add CI quality gates
  - [ ] 5.1 ci.yml with rust, ts and types jobs; read-only permissions; SHA-pinned actions
  - [ ] 5.2 deny.toml for licences, advisories, bans
  - _Requirements: 4.1, 4.2, 4.3, 4.4, 4.5_

- [ ] 6. Add open-source files
  - [ ] 6.1 README, CONTRIBUTING, SECURITY, CODE_OF_CONDUCT, issue and PR templates
  - [ ] 6.2 AGENTS.md (under 60 lines) and PRIVACY.md
  - [ ] 6.3 No LICENSE until ADR 0008 is Accepted
  - _Requirements: 6.1, 6.2, 6.3, 6.4_

- [ ] 7. Checkpoint: fresh clone passes every quality gate; record result in docs/progress.md
  - _Requirements: 1.1, 4.1_
