# 0006. Support passive and hosted capture, with content capture off by default

- Status: Accepted
- Date: 2026-09-26

## Context

Callsheet records what AI workers do without asking users to add an SDK to
their code. There are two ways to see a worker's activity:

- **Passive capture.** The user runs the agent as usual. Callsheet observes
  it from outside: the agent's model traffic goes through the Callsheet proxy
  (feature 03), and runtimes that emit OpenTelemetry send OTLP to Callsheet's
  receiver (feature 09). Callsheet does not start the agent and cannot stop
  it; it sees only what crosses those boundaries.
- **Hosted capture.** Callsheet starts the agent itself as an ACP client host
  (feature 10). It owns the session lifecycle, answers
  `session/request_permission`, and serves the agent's `fs/*` and
  `terminal/*` requests, so it can record and gate file and terminal
  actions as well as model calls.

Passive capture works with any agent and needs no change to how people work,
but it cannot enforce anything beyond the proxy. Hosted capture gives full
control and a complete record, but only for agents that speak ACP.

Message content is the most sensitive data Callsheet could hold. The OTel
GenAI conventions mark message content attributes as `Opt-In`
([ADR 0005](0005-protocol-pins.md)), so runtimes normally will not send it
over OTLP.

## Decision

- Callsheet supports both modes, and every recorded event states which mode
  produced it.
- Passive capture ships first (proxy in feature 03, OTLP ingest in feature
  09). Hosted capture arrives with the ACP host in feature 10.
- **Content capture is off by default in both modes.** By default Callsheet
  records metadata only: model, token counts, latency, status, trace and span
  ids, and cost. Storing message bodies requires an explicit user setting.
- When content capture is on, content comes from Callsheet's own proxy (or,
  in hosted mode, from the ACP stream), never from OTLP content attributes.
  Each message is stored once, content-addressed by SHA-256.
- API keys and auth headers are never recorded in either mode.

## Consequences

- Users get value with no setup change, and the privacy default matches the
  promise PRIVACY.md will make (feature 01, task 6.2): nothing leaves the
  machine, and nothing sensitive is kept unless asked.
- Replay (feature 15) needs content, so it only works for runs recorded with
  content capture on; the UI must say so rather than fail silently.
- Passive mode cannot block a file write or a shell command; only budgets and
  rate limits at the proxy are enforceable. Documentation must state which
  guarantees apply in which mode.
- Two ingestion paths (proxy and OTLP) can describe the same call; features
  03 and 09 must de-duplicate them, for example by trace and span id.

## Sources

- OTel GenAI conventions at the commit pinned in ADR 0005 — content
  attributes (`gen_ai.input.messages`, `gen_ai.output.messages`,
  `gen_ai.system_instructions`, `gen_ai.tool.definitions`) marked `Opt-In`:
  https://github.com/open-telemetry/semantic-conventions-genai/blob/e57c543b4889619eb2a05702471937db5119165d/docs/gen-ai/gen-ai-spans.md
- ACP v1 client methods (`session/request_permission`, `fs/read_text_file`,
  `fs/write_text_file`, `terminal/*`):
  https://github.com/agentclientprotocol/agent-client-protocol/blob/v1.9.1/docs/protocol/v1/overview.mdx
