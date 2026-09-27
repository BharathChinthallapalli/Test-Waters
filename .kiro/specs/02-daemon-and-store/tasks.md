# Tasks — 02 daemon-and-store

The owner approved the design (PR #42) and chose to implement it with parallel workers.
The layers of the design set the order: tasks in one wave are independent of each other
and each lands as its own pull request; a wave starts once the previous one is merged.

- [x] 0. Shared foundation (wave 0, one PR); 0.5 is still open
  - [x] 0.1 Every dependency from the design at its pinned version; `Cargo.lock`; `deny.toml` allows BSD-3-Clause (subtle, matchit) and Zlib (foldhash)
  - [x] 0.2 Wire types in `cs-core::rpc` and `cs-core::control` for every method, generated into `packages/api-types`
  - [x] 0.3 Module files with each module's contract, so every task below writes only its own files
  - [x] 0.4 `cs-store::fsperm`: owner-only directories and files on Unix, atomic replace
  - [ ] 0.5 Default control port, checked against the IANA registry
    BLOCKED: `www.iana.org` is denied by this environment's network policy. Needed by
    task 8. The owner either allows `www.iana.org` or picks the port.
  - _Requirements: 1.1, 1.4, 2.8, 7.1_

- [x] 1. Event hashing (wave 1, `event-hash`): `cs-core::event`, RFC 8785 vectors, number check
  - _Requirements: 4.3, 4.4, 4.7_
- [x] 2. Owner-only files on Windows (wave 1, `win-acl`): protected DACL; Windows CI job in **CI passed**
  - _Requirements: 1.4, 2.2, 7.2_
- [x] 3. Content key in the OS keychain (wave 1, `secrets`)
  - _Requirements: 5.3, 5.4_
- [x] 4. HTTP layer and JSON-RPC dispatch (wave 1, `rpc-http`)
  - _Requirements: 2.1, 2.3, 2.4, 2.6, 1.6_
- [x] 5. Schema, pragmas and migrations (wave 1, `schema`)
  - _Requirements: 3.1, 3.2, 3.3, 4.1_
- [x] 6. SECURITY.md, PRIVACY.md and README statements (wave 1, `docs`)
  - _Requirements: 4.6, 5.4, 6.5_
- [x] 7. Writer thread, append, read gate, content storage and the capture setting (wave 2, `writer`)
  - _Requirements: 3.4, 4.1, 4.2, 4.3, 4.4, 5.1, 5.2_
- [x] 8. Daemon process: config, binding, paths, lock, discovery, token, logs, shutdown (wave 2, `daemon-proc`)
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 2.2, 2.7, 2.9_
- [x] 9. Verification and `events.verify` (wave 3, `verify`)
  - _Requirements: 4.5, 4.6, 6.4_
- [x] 10. Erasure and the `content.*` methods (wave 3, `erase`)
  - _Requirements: 6.1, 6.2, 6.3, 6.4_
- [x] 11. Daemon wiring, remaining methods and the TypeScript client test (wave 3, `wire`)
  - _Requirements: 2.5, 2.8, 2.9, 3.2, 7.1_

- [x] 12. Desktop status screen: daemon state, health and version in both themes (wave 3, `status-screen`; added by the owner)
  - _Requirements: 2.3, 2.7, 2.9, 5.1_
- [x] 13. Checkpoint: every gate passes in CI on Linux and Windows; curl run and the status screen against the real daemon recorded in `docs/progress.md`
  - _Requirements: 7.1, 7.2_
