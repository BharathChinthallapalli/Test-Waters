# Design — 03 passthrough-proxy

Requirements and design in one file, kept short on purpose (#40). Wire facts are cited to
the official pages in "Sources"; anything not verified there is marked.

## Goal
Point Claude Code at Callsheet with one environment variable and have it keep working
exactly as before, while every call is recorded locally as metadata:

```sh
export ANTHROPIC_BASE_URL=http://127.0.0.1:<proxy port>
claude
```

Done means: a real Claude Code session runs through the proxy unchanged (streaming,
tool use, subscription or API-key login), and each call shows up in the desktop app's
**Recent calls** list with model, tokens, status and timing. No content is stored
unless capture is on. Nothing is sent anywhere except the provider the call was for.

Not in 03: budgets and 429 enforcement (05), rate limiting (06), OpenAI/Codex (#5),
cost in money (05's price table), signed checkpoints (04), OTLP (09).

## Requirements
1. **Transparent.** Request method, path, query, headers and body bytes reach the
   upstream unchanged, except the rules in "Headers". Response status, headers and
   body bytes reach the client unchanged and in order; SSE events are forwarded as
   they arrive, never buffered to the end. `anthropic-beta`, `anthropic-version` and
   every other `anthropic-*` header pass as open lists (#6). Error bodies pass
   unmodified.
2. **Credentials never stored or logged.** `x-api-key` and `Authorization` are
   forwarded and never written to the log, the store or a record. A test sends all
   three forms (#6) and searches the log output and the database for them.
3. **Recording never slows or breaks a call.** The response is teed: bytes go to the
   client first, a copy goes to a bounded observer. Records go through a bounded queue;
   when it's full the record is dropped and counted (`health.proxy.recordsDropped`).
   A store failure never fails the call.
4. **Correct usage.** From `message_start.message.usage`, overwritten by the last
   `message_delta.usage` (cumulative, never summed); from `usage` for a non-streamed
   response. Provider-reported only; never estimated (#21). Unknown SSE event types,
   `ping` and mid-stream `error` events are forwarded untouched; an `error` event sets
   the record's `errorType`.
5. **Local only.** The proxy listens on `127.0.0.1` only. A request with an `Origin`
   header, or a `Host` other than `127.0.0.1:<port>` / `localhost:<port>`, gets 403.
   It adds no credentials of its own, so it grants nothing a caller didn't bring.
6. **Stable address.** The first start picks a free loopback port and saves it in the
   data directory (`proxy-port`, owner-only); later starts reuse it and fail loudly
   if it's taken. `--proxy-listen 127.0.0.1:<port>` overrides. (No IANA-registered
   default: `www.iana.org` is blocked here, task 0.5 of feature 02.)
7. **Visible.** `calls.list` returns recorded calls newest first; the desktop app shows
   them and the exact `export ANTHROPIC_BASE_URL=…` line to copy.
8. **Content capture respected.** With capture off, no request or response body is
   kept, even in memory beyond the observer's bounded buffer. With capture on, the
   request body and the response body are stored as two content items (store caps:
   64 items, 8 MiB), or none if over the cap, with `contentTruncated: true`.

## Architecture
```
Claude Code ── http://127.0.0.1:<proxy port> ──► cs-daemon proxy listener (cs-proxy)
                                                   │  forward (reqwest, rustls/ring, HTTPS_PROXY honoured)
                                                   ├──────────────► https://api.anthropic.com
                                                   │  response bytes ──► client, in order
                                                   └─ copy ─► ResponseObserver ─► PendingCall ─► Recorder (bounded mpsc, drop+count)
                                                                                                  └─► Store::append("llm.call")
```
One binary: the proxy is a second listener inside `cs-daemon`, served by the same
hyper accept loop (header-read timeout 10 s) but **without** the control API's 10 s
request timeout; long streams run as long as the upstream sends. Upstream connect
timeout 10 s, no total timeout.

### Modules (`crates/cs-proxy/src/`)
| Module | Owns |
|---|---|
| `headers.rs` | request/response header rules below |
| `forward.rs` | the hyper service: guard (Origin/Host), buffer request body (≤ 32 MiB, else 413), send upstream, stream the response back while feeding the observer, build the `PendingCall`, capture copies when capture is on |
| `observe.rs` | `ResponseObserver`: incremental SSE decoder with a line cap, JSON body parse with a size cap, usage/model/stop reason/error/`request-id`/rate-limit header extraction |
| `recorder.rs` | `Recorder`: bounded queue, `try_send` drop+count, writer task appending `llm.call` events, drain on shutdown |
| `trace.rs` | W3C `traceparent` parse, or a generated trace id when absent (recorded, not forwarded) |

### Headers
- **To upstream:** all request headers except hop-by-hop (`connection`, `keep-alive`,
  `proxy-connection`, `proxy-authorization`, `te`, `trailer`, `transfer-encoding`,
  `upgrade`, and any named in `connection`), `host` (set to the upstream's), and
  `x-callsheet-*` (Callsheet's own, stripped). `accept-encoding` is removed so the
  upstream answers uncompressed and the observer can read usage; the client still
  gets a valid response (no `content-encoding`). Documented deviation.
- **To client:** all response headers except hop-by-hop.
- **Recorded** (allowlist, never auth): `request-id`, `retry-after`, `x-should-retry`,
  and every `anthropic-ratelimit-*` name with its raw value (#6, #39: raw now,
  parsing later).

### Run grouping and what is recorded
The run is, in order: `x-callsheet-run: <id>` (settable with Claude Code's
`ANTHROPIC_CUSTOM_HEADERS`; stripped before forwarding); else `cc-<x-claude-code-session-id>`
(Claude Code sends it on every request, one per session [GWP]; forwarded unchanged);
else the daemon's default run `proxy-<startedAtMs>`. An invalid value (empty, over
256 bytes, control characters) falls through to the next rule.

Every path is forwarded (Claude Code also sends `HEAD /api/hello` at startup and, when
enabled, `GET /v1/models` [GWP]); only requests whose path starts with `/v1/` are
recorded.

### Record (`llm.call` event body, `cs_core::llm::LlmCallRecord`)
Integers only (hash path, feature 02). `provider`, `method`, `path` (no query),
`status`, `outcome` (`completed`, `upstreamError`, `clientCancelled`,
`upstreamUnreachable`, `incomplete`), `streamed`, `model?`, `requestId?`,
`stopReason?`, `errorType?`, `usage?` {`inputTokens`, `outputTokens`,
`cacheCreationInputTokens`, `cacheReadInputTokens`}, `startedAtMs`, `ttfbMs?`,
`durationMs`, `requestBytes`, `responseBytes`, `rateLimitHeaders` (map),
`traceId`, `userAgent?` (≤ 200 bytes), `contentTruncated?`.

### Errors the proxy itself returns (Anthropic shape, so clients handle them)
`{"type":"error","error":{"type":"<type>","message":"<text>"}}`:
403 `permission_error` (guard), 413 `request_too_large`, 502 `api_error`
("Callsheet could not reach the provider: …", no secrets), 504 on connect timeout.

### Daemon wiring
`--proxy-listen`, `--proxy-upstream <https URL | http://127.0.0.1:port for tests>`.
`daemon.json` gains `proxyAddress`; `health` gains `proxy {address, callsRecorded,
recordsDropped}`. Shutdown: stop accepting on the proxy, let open calls finish up to
the drain grace, drain the recorder, then the existing steps.

## Testing
- **Golden transparency** (cs-proxy, mock upstream on loopback): non-streamed JSON;
  SSE with `ping`, an unknown event type and a mid-stream `error`; 429 with
  `retry-after` and a JSON error body; `anthropic-beta` with several values incl. an
  OAuth-style one reaches upstream byte for byte; response bytes identical.
- **Chunk timing:** the client receives the first SSE event before the upstream sends
  the last (no buffering).
- **Usage:** overwrite, not sum; missing usage → `usage: null`, never a guess.
- **Credentials:** all three auth forms absent from logs, records and the database.
- **Backpressure:** queue full → call still succeeds, `recordsDropped` increments.
- **Client cancel** mid-stream → upstream request dropped, record `clientCancelled`.
- **Guard:** `Origin` → 403; wrong `Host` → 403.
- **Real end-to-end** (manual, recorded in `progress.md`): Claude Code `claude -p`
  through the proxy; the call appears in `calls.list` and the desktop list.

## Sources
[`sources.md`](sources.md): every wire fact with its official URL, fetched 2026-09-27
(Claude Code gateway protocol, env vars, network config; Messages API, streaming,
errors, rate limits, versioning, token counting; W3C Trace Context from the working
group's spec source, since w3.org is blocked here). Issue inputs: #6, #21, #39.

Facts the design leans on hardest [GWP]: forward `anthropic-*` headers and body bytes
unchanged (stripping `anthropic-beta` → 401 on a claude.ai login; rewriting the body →
400); never buffer or drop SSE events (Claude Code aborts a stream silent for 300 s,
and treats a body that ends before `message_delta` as a dropped connection); inference
goes to `/v1/messages?beta=true`, so match on the path.
