# F03 facts sheet: local passthrough proxy for Claude Code (`ANTHROPIC_BASE_URL`)

Fetched 2026-09-27 via curl through the configured agent proxy, TLS verification on.
Docs pages were fetched as their `.md` variants (same URL + `.md`); cite the canonical URL.
Every bullet ends with a source key. UNVERIFIED = no official page reached for it.

## Sources (key = exact URL)

- [GW] https://code.claude.com/docs/en/llm-gateway
- [GWP] https://code.claude.com/docs/en/llm-gateway-protocol (gateway compatibility guide; linked from [GW])
- [GWC] https://code.claude.com/docs/en/llm-gateway-connect
- [ENV] https://code.claude.com/docs/en/env-vars (the settings page defers env vars here; see [SET])
- [SET] https://code.claude.com/docs/en/settings
- [MON] https://code.claude.com/docs/en/monitoring-usage (Traces (beta) section)
- [CCERR] https://code.claude.com/docs/en/errors
- [NET] https://code.claude.com/docs/en/network-config (Streaming idle watchdogs)
- [STR] https://platform.claude.com/docs/en/build-with-claude/streaming
- [MSG] https://platform.claude.com/docs/en/api/messages
- [OVR] https://platform.claude.com/docs/en/api/overview
- [BETA] https://platform.claude.com/docs/en/api/beta-headers
- [ERR] https://platform.claude.com/docs/en/api/errors
- [RL] https://platform.claude.com/docs/en/api/rate-limits (Response headers section)
- [VER] https://platform.claude.com/docs/en/api/versioning
- [TOK] https://platform.claude.com/docs/en/build-with-claude/token-counting
- [W3C-REQ] https://raw.githubusercontent.com/w3c/trace-context/main/spec/20-http_request_header_format.md
- [W3C-PM] https://raw.githubusercontent.com/w3c/trace-context/main/spec/30-processing-model.md
- [W3C-IDX] https://raw.githubusercontent.com/w3c/trace-context/main/index.html (respec config: `specStatus: "ED"`)
- BLOCKED (proxy 403 on CONNECT, also WebFetch EGRESS_BLOCKED): https://www.w3.org/TR/trace-context/ and https://w3c.github.io/trace-context/. W3C facts below come from the W3C WG's own spec source repo (Editor's Draft), not the published TR.

## 1. What Claude Code sends through a gateway

Endpoints
- Anthropic Messages format is selected by `ANTHROPIC_BASE_URL`; endpoints `/v1/messages` and `/v1/messages/count_tokens` (optional). [GWP]
- Token counting is the only optional endpoint; if absent, Claude Code falls back to a character-based estimate (`/context` shows approximate counts). [GWP]
- Match on path, not full URL: inference posts to `/v1/messages?beta=true`. [GWP]
- Startup: `HEAD /api/hello` connection-warming probe, best-effort, can be rejected harmlessly; skipped when an HTTP proxy or client cert is configured. [GWP]
- Model discovery (opt-in `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`): `GET /v1/models?limit=1000`, 3 s default timeout, any redirect (even http->https) = silent failure; reads `data[].id`, optional `display_name`, `description`; keeps ids containing `claude` or `anthropic` (case-insensitive). [GWP]
- Fast-mode availability check and WebFetch domain safety check go directly to `api.anthropic.com`, not through `ANTHROPIC_BASE_URL`. [GWP]
- Base URL with a path prefix (e.g. `http://127.0.0.1:8080/prefix`): not documented. UNVERIFIED.

Request headers (header names case-insensitive on the wire) [GWP]
- MUST forward unchanged: `anthropic-version` (currently `2023-06-01`) and `anthropic-beta`. Plus `anthropic-workspace-id` if upstream is Claude Platform on AWS. [GWP]
- `anthropic-beta`: comma-separated; forward verbatim, do not allowlist values (set changes per release). [GWP] Comma-separated multi-value format also in [BETA].
- With a claude.ai login (base URL set, no gateway credential), `anthropic-beta` carries an OAuth capability the upstream requires; stripping it -> `401`. [GWP] [GW]
- Treat `anthropic-*` headers and body fields as open lists; pass through rather than allowlist. [GWP]
- `Authorization`, `x-api-key`: the gateway credential, in one or both depending on the variable set. [GWP]
- `x-claude-code-session-id` (per session), `x-claude-code-agent-id` (subagent requests only), `x-claude-code-parent-agent-id` (nested agents only): consumable, need not be forwarded. [GWP]
- Gateway hint headers (`x-claude-code-request-class`, `-agent-type`, `-compaction`, `-context-compacted`, `-prev-tool-durations`, `-prompt-id`): OFF by default on a custom base URL; enable with `CLAUDE_CODE_GATEWAY_HINT_HEADERS=1` (v2.1.273+). [GWP] [ENV]
- Upstream API itself requires `anthropic-version` (Yes), `content-type: application/json` (Yes), `Authorization: Bearer` (Yes unless `x-api-key` set), `x-api-key` ("legacy fallback", still supported). [OVR] [VER]
- Invalid/unauthorized beta name -> `400 invalid_request_error` "Unexpected value(s) ... for the `anthropic-beta` header". [BETA]

Credential variables
- `ANTHROPIC_BASE_URL`: overrides the API endpoint to route through a proxy/gateway. Non-first-party host disables MCP tool search by default (`ENABLE_TOOL_SEARCH=true` if proxy forwards `tool_reference` blocks) and Remote Control (v2.1.196+). [ENV]
- `ANTHROPIC_AUTH_TOKEN` -> `Authorization: Bearer <value>` (prefix added by Claude Code). [ENV] [GWC]
- `ANTHROPIC_API_KEY` -> `x-api-key` (env-vars page writes `X-Api-Key`). [ENV] [GWC]
- `apiKeyHelper` -> value sent in BOTH `Authorization` and `x-api-key`. [GWC]
- Wrong variable = credential in a header the gateway doesn't read -> `401`. [GWC]
- `ANTHROPIC_AUTH_TOKEN` takes precedence immediately over a saved claude.ai login; `ANTHROPIC_API_KEY` prompts once in interactive mode. [GWC]
- Setting only `ANTHROPIC_BASE_URL` (no credential) keeps the claude.ai login active; traffic still routes through the gateway under subscription limits/billing. [GW]
- `ANTHROPIC_CUSTOM_HEADERS`: `Name: Value`, newline-separated (use `\n` in JSON settings); invalid header characters fail the request (v2.1.227+). [ENV] [GWC]
- For discovery requests, a non-empty custom header replaces a built-in header of the same name (case-insensitive). Same override on inference requests: not stated. UNVERIFIED. [GWP]

Body handling
- Feature header+body pairs travel together; stripping the header but passing the body -> hard `400`; content-inspection rewriting breaks it too: "inspect without modifying". [GWP]
- Forward `cache_control` unchanged; don't convert block-form `system`/message content to strings (else silent loss of prompt caching). [GWP]
- Forward `system` array exactly as received; first block is an attribution block that `api.anthropic.com` strips positionally. Reordering/prepending/merging defeats the strip. `CLAUDE_CODE_ATTRIBUTION_HEADER=0` omits it client-side. [GWP]
- Rewriting `system`, `tools`, or earlier `messages` can cause `400` "bound to a different conversation" (preserved-thinking check). [GWP]
- Fine-grained tool streaming is off by default on a custom base URL (`CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING=1`). [GWP]
- WAF body rules (XSS) can 403 real sessions; exempt `/v1/messages` from body inspection. [GWC]

Timeouts relevant to the proxy
- `API_TIMEOUT_MS` default 600000 (10 min). [ENV]
- Byte-level watchdog runs on custom `ANTHROPIC_BASE_URL`; aborts a stream silent for 300 s by default; every relayed byte (incl. `ping`, SSE comments) resets it. [GWP] [NET]
- First-byte deadline does NOT run when `ANTHROPIC_BASE_URL` routes through a gateway. [NET]
- `CLAUDE_STREAM_IDLE_TIMEOUT_MS` min 300000 when set explicitly. [ENV]

## 2. SSE wire format

- Request with `"stream": true` -> server-sent events. [STR]
- Response content type: `text/event-stream` on streamed Anthropic-format responses (gateway must return it). [GWP] The platform streaming page itself does not state the content type. [STR]
- Each event: SSE `event: <name>` line + `data:` JSON whose `type` equals the event name. [STR]
- Order: `message_start` (Message, empty `content`) -> per content block: `content_block_start`, 1+ `content_block_delta`, `content_block_stop` (each with `index` into final `content`) -> 1+ `message_delta` -> `message_stop`. [STR]
- Exception: server-side fallback emits a `fallback` block as start/stop with no deltas. [STR]
- `ping` events: any number, anywhere (`data: {"type": "ping"}`). [STR]
- Unknown event types may be added; handle gracefully. [STR] [VER]
- API version `2023-06-01`: all events are named events; no `data: [DONE]` terminator. [VER]
- Delta types: `text_delta`(`text`), `input_json_delta`(`partial_json`, partial JSON strings), `thinking_delta`(`thinking`), `signature_delta`(`signature`, just before `content_block_stop`), `citations_delta`. [STR] [MSG]
- Usage: `message_start.message.usage` (e.g. `input_tokens`, `output_tokens`, cache fields). [STR] [MSG]
- `message_delta.usage` (MessageDeltaUsage): `input_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`, `output_tokens`, `output_tokens_details`, `server_tool_use`; documented as CUMULATIVE ("The token counts shown in the `usage` field of the `message_delta` event are *cumulative*"). [STR] [MSG]
- `message_delta.delta`: `stop_reason`, `stop_sequence`, `stop_details`, `container`. [MSG]
- `stop_reason` is null in `message_start`, non-null otherwise. [MSG]
- A later usage-only `message_delta` with `stop_reason: null`/absent does not clear an earlier one (Claude Code behavior). [GWP]
- Mid-stream errors: `event: error` / `data: {"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}`; arrives after an HTTP 200, so standard status handling doesn't apply. [STR] [ERR]
- Proxy rules from Claude Code: stream, don't buffer (buffering stalls Claude Code); don't drop, duplicate or reorder events; relay through final `message_delta` + `message_stop`. [GWP]
- Clean body end after a block started but before a `message_delta` with `stop_reason` = treated as dropped connection. [GWP]
- Forward `ping`s; when translating from a ping-less upstream, emit your own `ping`s during silent gaps. [GWP]

## 3. Non-streaming response (Message object) [MSG]

- `id` (string; format/length may change), `type: "message"`, `role: "assistant"`, `content[]`, `model`, `stop_reason`, `stop_sequence`, `stop_details`, `container`, `diagnostics`, `usage`.
- `stop_reason` values: `end_turn`, `max_tokens`, `stop_sequence`, `tool_use`, `pause_turn`, `refusal`, `model_context_window_exceeded`; always non-null when non-streaming.
- `usage`: `input_tokens`, `output_tokens`, `cache_creation_input_tokens` (nullable), `cache_read_input_tokens` (nullable), `cache_creation` {`ephemeral_5m_input_tokens`, `ephemeral_1h_input_tokens`}, `output_tokens_details` {`thinking_tokens`}, `server_tool_use` {`web_search_requests`, `web_fetch_requests`}, `service_tier` (`standard`|`priority`|`batch`|null), `inference_geo`.
- Total input = `input_tokens` + `cache_creation_input_tokens` + `cache_read_input_tokens`.
- `output_tokens` is the inclusive billing total; `thinking_tokens` <= `output_tokens`.
- Long non-streaming requests (large `max_tokens`, >10 min) risk idle-connection drops; streaming recommended. [ERR]

## 4. Response headers

- `request-id`: on every API response, e.g. `req_018EeWyXxfu5pfWkrYcMdjWG`; equals `request_id` in error bodies. [ERR] [OVR]
- `anthropic-organization-id`, `anthropic-workspace-id` (response). [OVR]
- `retry-after`: seconds to wait; not sent with the spend-cap 429. [RL]
- `anthropic-ratelimit-{requests,tokens,input-tokens,output-tokens}-{limit,remaining,reset}`; `*-reset` is RFC 3339; token `remaining` rounded to nearest thousand; `tokens-*` shows the most restrictive limit in effect. [RL]
- `anthropic-priority-{input,output}-tokens-{limit,remaining,reset}` (Priority Tier only). [RL]
- Claude Code reads: `content-type` (stall detection), `retry-after` (integer seconds, not HTTP date; >60 stops retries outside `CLAUDE_CODE_RETRY_WATCHDOG`), `x-should-retry` (`true`/`false`, pass through), `anthropic-ratelimit-unified-*` (forward unchanged on every response). [GWP]
- Unified headers: used for claude.ai plan-usage display and, on a 429, to tell a plan limit/spend cap from a temporary throttle (absence => "Server is temporarily limiting requests"). [GWP] [CCERR]
- Individual `anthropic-ratelimit-unified-*` header names/semantics: not documented on any fetched page. UNVERIFIED.
- Forward error response bodies unmodified; Claude Code's recovery matches upstream error wording (wrapping breaks it unless message carries `capability_rejected:` token). [GWP]

## 5. Errors [ERR]

- Shape: `{"type":"error","error":{"type":"...","message":"..."},"request_id":"req_..."}`; `type` values may grow.
- 400 `invalid_request_error` (also other 4XX; also org/workspace spend limit you set)
- 401 `authentication_error`
- 402 `billing_error`
- 403 `permission_error`
- 404 `not_found_error`
- 409 `conflict_error`
- 413 `request_too_large` (Messages and Token Counting: 32 MB; returned by Cloudflare before API)
- 429 `rate_limit_error` (rate limit, tier spend cap, Claude Code workspace spend limit)
- 500 `api_error`
- 504 `timeout_error`
- 529 `overloaded_error` (API temporarily overloaded; high traffic across all users)
- Spend-cap 429: no `retry-after`; `error.details.error_code: "enforced_spend_limit_reached"`. [RL]
- Streaming: error can occur after 200; see SSE `error` event. [ERR] [STR]
- Claude Code retries 5xx/overloaded/timeouts before any content streamed, up to 10 times (`CLAUDE_CODE_MAX_RETRIES`); no re-run after a completed text/tool block (avoids double tool execution). [CCERR]

## 6. count_tokens [MSG] [TOK]

- `POST /v1/messages/count_tokens`; same inputs as create (messages, system, tools, thinking, images, PDFs). [MSG] [TOK]
- Response: `{"input_tokens": <int>}` (MessageTokensCount; total across messages, system, tools). [MSG]
- Count is an estimate; may differ slightly from actual. No caching logic applied. [TOK]
- Free, but separate RPM rate limits, independent from message creation. [TOK]

## 7. W3C `traceparent`

Claude Code behavior [MON] [ENV]
- Tracing requires `CLAUDE_CODE_ENABLE_TELEMETRY=1` + `CLAUDE_CODE_ENHANCED_TELEMETRY_BETA=1` + `OTEL_TRACES_EXPORTER`. [MON]
- With tracing on and direct API connection, each model request carries `traceparent` = `claude_code.llm_request` span context; API's `traceresponse` recorded as span link. [MON]
- By default NOT sent when `ANTHROPIC_BASE_URL` points at a custom proxy; set `CLAUDE_CODE_PROPAGATE_TRACEPARENT=1` (v2.1.152+). [MON] [ENV]
- => A local proxy will usually see NO inbound `traceparent`; it must generate one if it wants traces. [MON]

Format (W3C Editor's Draft source; TR not reachable) [W3C-REQ] [W3C-IDX]
- Header name `traceparent`, ASCII case-insensitive; SHOULD send lowercase.
- `version "-" trace-id "-" parent-id "-" trace-flags`, e.g. `00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01`.
- `version` 2 lowercase hex (`00`; `ff` invalid); `trace-id` 32 lowercase hex, all-zero invalid; `parent-id` 16 lowercase hex, all-zero invalid; `trace-flags` 2 hex (bit0 `01` sampled; bit1 `02` random-trace-id in ED; others MUST be 0).
- Invalid `trace-id`/`parent-id` -> ignore the whole header.
- Pass-through services should not analyze version; allow larger future headers, reject only prohibitively large ones.
- Random flag bit (`02`) exists in the ED; whether it is in the published TR: UNVERIFIED.

Proxy processing rules [W3C-PM]
- None received: create new `trace-id` + `parent-id`; `tracestate` without `traceparent` is invalid, MUST be discarded; set both on outgoing request.
- Received but unparseable/invalid: create new `traceparent`, delete `tracestate`.
- Received and valid (participating vendor): MUST set `parent-id` to current operation's id; MAY update sampled flag; MAY add/update `tracestate` keys (moved to left); SHOULD NOT delete others' keys.
- Alternative: "Proxies or messaging middleware MAY decide not to modify the `traceparent` headers but remove invalid headers or add additional information to `tracestate`."
- If `traceparent` fails to parse, MUST NOT parse `tracestate`. [W3C-REQ]
- `traceresponse` response header exists in the ED (restarted trace / deferred sampling). [W3C-PM]

## 8. Gaps / UNVERIFIED

- W3C published TR (https://www.w3.org/TR/trace-context/) and https://w3c.github.io/trace-context/: blocked (403). All W3C facts are from the ED source on raw.githubusercontent.com; differences vs TR Level 1/2 UNVERIFIED.
- Names/meanings of individual `anthropic-ratelimit-unified-*` headers: UNVERIFIED (only the wildcard is documented).
- Whether `ANTHROPIC_BASE_URL` may contain a path prefix: UNVERIFIED.
- Whether `ANTHROPIC_CUSTOM_HEADERS` overrides built-in headers on inference requests (only stated for discovery): UNVERIFIED.
- Explicit `content-type: text/event-stream` on the platform streaming/Messages pages: not stated there; only in [GWP].
- Upstream API behavior on an unknown extra header (e.g. forwarded `x-claude-code-*`, `traceparent`): not documented; [GWP] only says the gateway "need not forward" them. UNVERIFIED.
- Settings page env-vars section: [SET] now links to [ENV]; facts taken from [ENV].
