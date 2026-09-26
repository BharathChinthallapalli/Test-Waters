# 0005. Pin the protocol and specification versions Callsheet implements

- Status: Accepted
- Date: 2026-09-26

## Context

Callsheet sits between agents and model providers and speaks several open
protocols. Several of them are still moving: the OpenTelemetry GenAI
conventions have Development status, MCP publishes dated revisions, and ACP
has a stable v1 alongside an alpha v2 schema. Code written against "latest"
drifts silently. Each pin below names the exact version and where it was read.

A notable finding: at semantic-conventions v1.44.0 the GenAI conventions
have **moved** to a separate repository,
`open-telemetry/semantic-conventions-genai`, which has no release tags yet.
The GenAI pin is therefore a commit of that repository.

## Decision

| Area | Pin | Status at pin |
|---|---|---|
| OTLP | opentelemetry-proto `v1.11.0`, commit `790608c4d51e6ffc12210b541e8514cbed9e91a4` | Stable for traces, metrics and logs; profiles Development |
| OTel semantic conventions (core) | `v1.44.0`, commit `e10a930844c6951757a43b849d364f7d056ac32b` | Mixed, per convention |
| OTel GenAI semantic conventions | semantic-conventions-genai commit `e57c543b4889619eb2a05702471937db5119165d` (2026-09-24) | Development |
| MCP | revision `2026-07-28`, tag `2026-07-28`, commit `5f5440bb26a62e2cf3440b92da5a667efa03b267` | Current revision (`LATEST_PROTOCOL_VERSION`) |
| ACP | wire protocol version `1`; JSON Schema `schema-v1.23.0` (commit `6d08f412a7a1370d3cc9a124e3be3d6acf92641e`); read at repo tag `v1.9.1` (commit `7e87dc205a7325bd07d0249fd20bb7486ee6ba95`) | Stable; v2 schema is `2.0.0-alpha.5` and is not used |
| W3C Trace Context | Trace Context Level 1 (W3C Recommendation), `traceparent` version `00` | Recommendation |
| Rate limiting | HTTP 429 (RFC 6585 §4) with `Retry-After` (RFC 9110 §10.2.3) | Both Standards Track |
| Rate-limit headers | `draft-ietf-httpapi-ratelimit-headers-11`, informational only | Internet-Draft |
| Attestations | in-toto Statement v1 (`_type` `https://in-toto.io/Statement/v1`), attestation repo `v1.2.0` | Stable |
| Envelope | DSSE protocol v1.0.2 (PAE prefix `DSSEv1`) | Stable |
| Log checkpoints | C2SP `tlog-checkpoint/v1.0.0` and `signed-note/v1.0.0`, commit `99fd482db06deca04d3fc80446a030ff9a947e97` | v1.0.0 |
| Canonical JSON | RFC 8785 (JSON Canonicalization Scheme) | Published RFC |

Rules that follow from these pins:

- **GenAI telemetry.** Use the `gen_ai.*` attributes as defined at the pinned
  commit (for example `gen_ai.operation.name`, `gen_ai.provider.name`,
  `gen_ai.request.model`, `gen_ai.response.model`, `gen_ai.usage.input_tokens`,
  `gen_ai.usage.output_tokens`, `gen_ai.response.id`,
  `gen_ai.response.finish_reasons`). The conventions define no cost
  attribute, so Callsheet's cost fields live under `callsheet.cost.*` and
  never under `gen_ai.*`. Message content (`gen_ai.input.messages`,
  `gen_ai.output.messages`, `gen_ai.system_instructions`,
  `gen_ai.tool.definitions`) is `Opt-In` in the conventions, so content for
  replay comes from Callsheet's own proxy capture, which is off by default
  ([ADR 0006](0006-capture-modes.md)).
- **MCP.** Revision 2026-07-28 is stateless: there is no `initialize`
  handshake, and each request carries its protocol version in
  `_meta` (`io.modelcontextprotocol/protocolVersion`), with `server/discover`
  for version selection. Trace context travels in `_meta` keys
  (`traceparent`, `tracestate`). The MCP proxy (feature 09) must not pass
  through tokens it received and must avoid the confused-deputy problem, as
  the revision's security considerations require.
- **ACP.** Callsheet negotiates `protocolVersion` 1 during `initialize` and
  uses JSON-RPC 2.0 over stdio, newline-delimited. Wire compatibility is
  judged by the negotiated protocol version, not by schema or crate versions.
- **Rate limits.** Enforcement answers with 429 and `Retry-After`. RateLimit
  and RateLimit-Policy headers from draft-11 may be added as information for
  clients; nothing depends on them. When both are sent, `Retry-After` takes
  precedence, as draft-11 requires.
- **Trace Context.** Propagate `traceparent` version `00`: 32 lowercase hex
  trace-id, 16 lowercase hex parent-id, 2 hex trace-flags; all-zero ids are
  invalid.
- **Watch, don't depend.** The agent-identity individual drafts (AIMS, AIP,
  PEDIGREE) are not pinned; nothing may depend on them.

Changing any pin requires a new ADR (or an update to this one) that cites the
new version and source, plus tests updated in the same change.

## Consequences

- Behaviour is reproducible against named versions, and upgrades are
  deliberate, reviewable changes.
- The GenAI conventions are Development status and live in a young
  repository; expect renames and re-pin when the upstream project tags a
  release.
- MCP 2026-07-28 differs sharply from 2025-11-25 (no sessions, no
  `initialize`, multi round-trip requests). Supporting older servers would
  need a separate compatibility path and is out of scope unless a feature
  asks for it.
- Pinned fields must be covered by golden tests in the features that
  implement them (03, 04, 06, 09, 10).

## Sources

Every pin was read from the specification's source at the stated tag or
commit (the public documentation sites were not reachable from the authoring
environment, so the Git sources were used):

- OTLP: https://opentelemetry.io/docs/specs/otlp/ — source
  https://github.com/open-telemetry/opentelemetry-proto/blob/v1.11.0/docs/specification.md
  (status lines; default ports 4317 gRPC and 4318 HTTP; paths `/v1/traces`,
  `/v1/metrics`, `/v1/logs`; `application/x-protobuf` and `application/json`).
- Semantic conventions v1.44.0:
  https://github.com/open-telemetry/semantic-conventions/blob/v1.44.0/docs/gen-ai/README.md
  (states that the GenAI conventions moved).
- GenAI conventions:
  https://github.com/open-telemetry/semantic-conventions-genai/blob/e57c543b4889619eb2a05702471937db5119165d/docs/gen-ai/gen-ai-spans.md
  and `docs/gen-ai/README.md` at the same commit (`Status: Development`;
  content attributes marked `Opt-In`).
- MCP: https://modelcontextprotocol.io/specification/2026-07-28 — source
  https://github.com/modelcontextprotocol/modelcontextprotocol/tree/2026-07-28/docs/specification/2026-07-28
  (`changelog.mdx`, `basic/authorization/security-considerations.mdx`,
  `basic/transports/streamable-http.mdx`) and `schema/2026-07-28/schema.ts`
  (`LATEST_PROTOCOL_VERSION = "2026-07-28"`).
- ACP: https://agentclientprotocol.com — source
  https://github.com/agentclientprotocol/agent-client-protocol/blob/v1.9.1/README.md
  ("The current stable ACP protocol version is `1`"),
  `docs/protocol/v1/transports.mdx`, `docs/protocol/v1/overview.mdx`,
  `schema/v1/Cargo.toml` (`1.23.0`) and `schema/v1/meta.json` (`"version": 1`).
- W3C Trace Context: https://www.w3.org/TR/trace-context-1/ — source
  https://github.com/w3c/trace-context/blob/acab820be9db7b3433668baa5cdd43f57f4c4be0/README.md
  ("Trace Context v1 has W3C Recommendation status") and
  `spec/20-http_request_header_format.md` (traceparent grammar for version
  `00`, read from the editor's draft at that commit).
- RFC 6585 §4 (429): https://www.rfc-editor.org/rfc/rfc6585#section-4 —
  read from https://github.com/httpwg/httpwg.github.io/blob/main/specs/rfc6585.html
- RFC 9110 §10.2.3 (Retry-After): https://www.rfc-editor.org/rfc/rfc9110#section-10.2.3 —
  read from https://github.com/httpwg/httpwg.github.io/blob/main/specs/rfc9110.html
- RateLimit headers draft-11:
  https://datatracker.ietf.org/doc/draft-ietf-httpapi-ratelimit-headers/11/ —
  source https://github.com/ietf-wg-httpapi/ratelimit-headers/blob/draft-ietf-httpapi-ratelimit-headers-11/draft-ietf-httpapi-ratelimit-headers.md
  (commit `9b4bc45c6be50e3e2455e9d6835b7537698d8ec4`).
- in-toto Statement v1:
  https://github.com/in-toto/attestation/blob/v1.2.0/spec/v1/statement.md
- DSSE v1.0.2:
  https://github.com/secure-systems-lab/dsse/blob/v1.0.2/protocol.md and
  `envelope.md`.
- C2SP: https://c2sp.org/tlog-checkpoint and https://c2sp.org/signed-note —
  source https://github.com/C2SP/C2SP/blob/tlog-checkpoint/v1.0.0/tlog-checkpoint.md
  and `signed-note.md` at the same tag.
- RFC 8785: https://www.rfc-editor.org/rfc/rfc8785 — draft source
  https://github.com/cyberphone/ietf-json-canon (README: "Completed:
  https://tools.ietf.org/html/rfc8785").
