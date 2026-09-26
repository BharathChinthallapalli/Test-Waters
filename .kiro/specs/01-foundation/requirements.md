# Requirements — 01 foundation

## Introduction
Create an empty but production-grade repository skeleton: builds, lints, tests and CI pass on
a clean clone, the desktop shell is hardened from its first commit, architecture decisions are
recorded, and outside contributors know the rules. No product behaviour ships in this feature.

## Requirement 1: Workspaces
**User story:** As a contributor, I want one command per language to build everything, so that
I can start working without setup guesswork.

1. WHEN a contributor runs `cargo build --workspace` on a clean clone THE SYSTEM SHALL build crates cs-core, cs-store, cs-proxy and cs-daemon without errors.
2. THE SYSTEM SHALL pin the Rust toolchain in `rust-toolchain.toml`.
3. WHEN a contributor runs `pnpm install` THE SYSTEM SHALL install apps/* and packages/* as one workspace.
4. THE SYSTEM SHALL ignore build outputs, dependencies and local databases in git.

## Requirement 2: Hardened desktop shell
**User story:** As a user, I want the desktop app secure by default, so that a compromised page
cannot reach my machine.

1. WHEN the app creates a window THE SYSTEM SHALL set contextIsolation true, sandbox true, nodeIntegration false and webSecurity true.
2. IF the renderer attempts to navigate away or open a new window THEN THE SYSTEM SHALL block it.
3. THE SYSTEM SHALL apply a Content-Security-Policy that forbids inline and remote scripts.
4. THE SYSTEM SHALL expose only an explicitly typed, empty preload API in this feature.

## Requirement 3: Generated shared types
**User story:** As a contributor, I want TS types generated from Rust, so that the two never drift.

1. WHEN a Rust type in cs-core is marked for export THE SYSTEM SHALL generate a matching TS type in packages/api-types.
2. IF committed generated types differ from a fresh generation THEN CI SHALL fail.
3. THE SYSTEM SHALL record the chosen generator and its source docs in an ADR.

## Requirement 4: CI quality gates
**User story:** As a maintainer, I want every change checked automatically, so that main never breaks.

1. WHEN a push or pull request occurs THE CI SHALL run all quality gates listed in steering/workflow.md.
2. IF any gate fails THEN THE CI SHALL report failure and block merge.
3. THE CI SHALL pin every third-party action by full commit SHA.
4. THE CI SHALL default the workflow token to read-only permissions.
5. THE SYSTEM SHALL configure cargo-deny for licences, advisories and bans.

## Requirement 5: Architecture decisions
**User story:** As a future session with no memory, I want decisions written down, so that I don't reopen them.

1. THE SYSTEM SHALL contain ADRs for: daemon not sidecar, SQLite owned by the daemon, JSON-RPC 2.0 control API with token, Biome over ESLint, protocol pins, capture modes, identity and log integrity.
2. THE SYSTEM SHALL contain a licence ADR with status Proposed listing MIT and Apache-2.0.
3. WHERE an ADR pins a protocol or spec version THE ADR SHALL cite the exact version or commit and source URL.

## Requirement 6: Open-source files
**User story:** As an outside contributor, I want the usual project files, so that I know how to help safely.

1. THE SYSTEM SHALL contain README, CONTRIBUTING, SECURITY (private vulnerability reporting), CODE_OF_CONDUCT, AGENTS.md, PRIVACY.md, issue and PR templates.
2. THE PRIVACY.md SHALL state that nothing leaves the machine by default and any future telemetry is opt-in.
3. THE AGENTS.md SHALL stay under 60 lines and point to .kiro/steering.
4. IF the licence decision is still Proposed THEN THE SYSTEM SHALL NOT add a LICENSE file.
