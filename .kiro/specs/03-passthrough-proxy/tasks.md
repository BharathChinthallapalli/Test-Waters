# Tasks — 03 passthrough-proxy

Waves: tasks in a wave touch separate files and land as separate pull requests; a wave
starts once the previous one is merged. Requirement numbers refer to `design.md`.

- [x] 0. Foundation (wave 0): this spec and `sources.md`; `cs-core::llm` record and
  `calls.list` types, `Discovery.proxyAddress`, `health.proxy`; `cs-proxy` module
  contracts and stubs; reqwest (rustls + ring) and `deny.toml` (ISC, CDLA-Permissive-2.0)

- [ ] 1. `forward` (wave 1): `cs-proxy` `headers.rs`, `trace.rs`, `forward.rs`: guard,
  header rules, run grouping, streaming passthrough with the observer tee, client
  cancel, capture copies, proxy errors; golden transparency tests against a mock
  upstream — _Requirements: 1, 2, 5, 8_
- [ ] 2. `observe` (wave 1): `cs-proxy` `observe.rs`: SSE decoder and JSON body with caps,
  usage overwrite rule, model, stop reason, errors, recorded headers; fixtures for
  ping, unknown event, mid-stream error — _Requirements: 4_
- [ ] 3. `recorder` (wave 1): `cs-proxy` `recorder.rs`: bounded queue, drop and count,
  `llm.call` appends, drain on shutdown — _Requirements: 3_
- [ ] 4. `calls-list` (wave 1): `cs-daemon` `methods/calls.rs` and its registration:
  `calls.list` newest first with paging — _Requirements: 7_
- [ ] 5. `desktop-calls` (wave 1): desktop Recent calls list and the
  `ANTHROPIC_BASE_URL` line to copy, against the fake daemon — _Requirements: 7_
- [ ] 6. `egress-gate` (wave 1, issue #56): CI job that starts the built desktop app
  behind a logging proxy and fails on any outbound request
- [ ] 7. `win-data-dir` (wave 1, issue #44): Windows data-directory owner and DACL check
  at startup

- [ ] 8. `wire` (wave 2): `cs-daemon` flags, `proxy-port` file, proxy listener,
  recorder lifecycle in shutdown, `health.proxy`, `daemon.json` `proxyAddress`; TypeScript
  integration test through the proxy to a mock upstream — _Requirements: 3, 5, 6, 7_

- [ ] 9. Checkpoint (wave 3): real Claude Code run through the proxy, recorded in
  `progress.md`, with the rate-limit headers actually seen (issue #39); PRIVACY.md and
  README updated
