// A stand-in daemon for rendering the status screen's states by hand. Not part
// of the app and never shipped: the build only compiles src/.
//
//   node scripts/fake-daemon.ts --data-dir <dir> [--mode <mode>] [options]
//
// It writes daemon.json, daemon.lock and control-token into <dir> the way
// cs-daemon does, with its own pid, and serves POST /rpc on 127.0.0.1 with the
// same Host and bearer checks. Modes:
//   healthy        `version`, `health` with a proxy, and `calls.list` (default)
//   older          `health` without a proxy and no `calls.list`, like a daemon
//                  before feature 03
//   no-health      `health` answers -32601, like a daemon before task 11
//   unhealthy      `health` answers a JSON-RPC error (-32000)
//   unauthorized   accepts no token: every request gets 401
//   unreachable    publishes a port nothing listens on
//   hang           accepts connections and never answers
// Options for `healthy`: --capture on|off, --events <n>, --uptime-ms <n>,
// --erasure-pending, --proxy-address <127.0.0.1:port>, --dropped <n>,
// --calls <n> (recorded calls, newest first, served 50 to a page; 0 by
// default), --calls-error (`calls.list` answers -32000), --unreadable <list>
// (indexes of calls, newest 0, such as "3,50-99", that `calls.list` skips as
// unreadable records: they still count towards a page, so "0-49" makes the
// newest page empty with a `nextBefore`).
//
// The calls are made up but shaped like the proxy's records: completed streams
// with prompt-cache reads, a 429 with `retry-after` and rate-limit headers, a
// 529, a cancelled stream, a stream cut off part-way, a `count_tokens` call
// (no model, no usage) and a long model name. No proxy is actually served.
import { randomBytes } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { parseArgs } from "node:util";

const { values } = parseArgs({
  options: {
    "data-dir": { type: "string" },
    mode: { type: "string", default: "healthy" },
    capture: { type: "string", default: "off" },
    events: { type: "string", default: "12408" },
    "uptime-ms": { type: "string", default: String((2 * 60 + 14) * 60_000) },
    "erasure-pending": { type: "boolean", default: false },
    "proxy-address": { type: "string", default: "127.0.0.1:41733" },
    dropped: { type: "string", default: "0" },
    calls: { type: "string", default: "0" },
    "calls-error": { type: "boolean", default: false },
    unreadable: { type: "string" },
  },
});

const dataDir = values["data-dir"];
if (!dataDir) {
  console.error("usage: fake-daemon.ts --data-dir <dir> [--mode <mode>]");
  process.exit(2);
}
const mode = values.mode;
const token = randomBytes(32).toString("hex");
// daemon.json gets the real start time: the app checks it against when this
// process started. The longer --uptime-ms is only what `health` reports.
const startedAtMs = Date.now();
const reportedStartMs = startedAtMs - Number(values["uptime-ms"]);

function reply(id: unknown, body: Record<string, unknown>): string {
  return JSON.stringify({ jsonrpc: "2.0", id, ...body });
}

const SECOND = 1000;
const MINUTE = 60 * SECOND;

/** Rate-limit headers as the Messages API sends them (docs: rate limits). */
function rateLimits(remaining: number, resetAtMs: number) {
  const reset = new Date(resetAtMs).toISOString().replace(/\.\d+Z$/, "Z");
  return {
    "anthropic-ratelimit-requests-limit": "50",
    "anthropic-ratelimit-requests-remaining": String(remaining),
    "anthropic-ratelimit-requests-reset": reset,
    "anthropic-ratelimit-input-tokens-limit": "30000",
    "anthropic-ratelimit-input-tokens-remaining": remaining > 0 ? "27000" : "0",
    "anthropic-ratelimit-input-tokens-reset": reset,
    "anthropic-ratelimit-output-tokens-limit": "8000",
    "anthropic-ratelimit-output-tokens-remaining": "8000",
    "anthropic-ratelimit-output-tokens-reset": reset,
  };
}

function hex(length: number, seed: number): string {
  let out = "";
  let state = seed * 2_654_435_761;
  while (out.length < length) {
    state = (state * 1_103_515_245 + 12_345) % 2 ** 31;
    out += state.toString(16);
  }
  return out.slice(0, length);
}

/** The i-th newest made-up call, `startedAtMs` ago-relative to `now`. */
function makeCall(i: number, now: number) {
  // Newest first: a few seconds apart, then minutes, then days back.
  const ago =
    i < 8 ? 20 * SECOND + i * 95 * SECOND : 15 * MINUTE + (i - 8) * 47 * MINUTE;
  const startedAtMs = now - ago;
  const requestId = `req_011C${hex(20, i + 1).toUpperCase()}`;
  const base = {
    provider: "anthropic",
    method: "POST",
    path: "/v1/messages",
    status: 200,
    outcome: "completed",
    streamed: true,
    model: "claude-sonnet-4-5-20250929",
    requestId,
    stopReason: "tool_use",
    usage: {
      inputTokens: 3 + (i % 7) * 211,
      outputTokens: 180 + ((i * 37) % 900),
      cacheCreationInputTokens: i % 5 === 0 ? 2_314 : 0,
      cacheReadInputTokens: 48_210 + i * 613,
    },
    startedAtMs,
    ttfbMs: 380 + ((i * 53) % 900),
    durationMs: 2_400 + ((i * 1_217) % 21_000),
    requestBytes: 180_000 + i * 900,
    responseBytes: 4_200 + i * 31,
    rateLimitHeaders: {
      "request-id": requestId,
      ...rateLimits(49 - (i % 10), startedAtMs + MINUTE),
    } as Record<string, string>,
    traceId: hex(32, i + 101),
    userAgent: "claude-cli/2.1.227 (external, cli)",
  };
  switch (i % 11) {
    case 1: // prompt-cache write, end of turn
      return {
        ...base,
        stopReason: "end_turn",
        model: "claude-opus-4-1-20250805",
      };
    case 2: // the token-count endpoint: no model, no usage in its reply
      return {
        ...base,
        path: "/v1/messages/count_tokens",
        streamed: false,
        model: undefined,
        stopReason: undefined,
        usage: undefined,
        ttfbMs: 140,
        durationMs: 161,
      };
    case 3: // rate limited
      return {
        ...base,
        status: 429,
        outcome: "upstreamError",
        streamed: false,
        model: undefined,
        stopReason: undefined,
        errorType: "rate_limit_error",
        usage: undefined,
        ttfbMs: 212,
        durationMs: 214,
        rateLimitHeaders: {
          "request-id": requestId,
          "retry-after": "17",
          "x-should-retry": "true",
          ...rateLimits(0, startedAtMs + 17 * SECOND),
        },
      };
    case 5: // cancelled by the user part-way through a stream
      return {
        ...base,
        outcome: "clientCancelled",
        stopReason: undefined,
        usage: { ...base.usage, outputTokens: 41 },
        durationMs: 6_812,
      };
    case 6: // a long model name, through a gateway-style id
      return {
        ...base,
        model: "us.anthropic.claude-sonnet-4-5-20250929-v1:0-extended-context",
        stopReason: "end_turn",
      };
    case 7: // the connection dropped before the stream ended
      return {
        ...base,
        outcome: "incomplete",
        stopReason: undefined,
        usage: { ...base.usage, outputTokens: 96 },
        durationMs: 31_406,
      };
    case 9: // overloaded
      return {
        ...base,
        status: 529,
        outcome: "upstreamError",
        streamed: false,
        model: undefined,
        stopReason: undefined,
        errorType: "overloaded_error",
        usage: undefined,
        ttfbMs: 1_020,
        durationMs: 1_024,
        rateLimitHeaders: { "request-id": requestId, "x-should-retry": "true" },
      };
    case 10: // a quick one, with Haiku
      return {
        ...base,
        model: "claude-haiku-4-5-20251001",
        stopReason: "end_turn",
        usage: { ...base.usage, outputTokens: 12 },
        durationMs: 842,
      };
    default:
      return base;
  }
}

/** Every recorded call, newest first, at positions interleaved with events. */
const allCalls = Array.from({ length: Number(values.calls) }, (_, i) => ({
  globalPos: Number(values.events) - 3 * i,
  runId: i < 30 ? "cc-5c1f0d2e" : "cc-9a77b4c0",
  call: makeCall(i, startedAtMs),
}));

/** Indexes into `allCalls` of records to treat as unreadable ("3,50-99"). */
const unreadable = new Set(
  (values.unreadable ?? "")
    .split(",")
    .filter(Boolean)
    .flatMap((part) => {
      const [from, to = from] = part.split("-").map(Number);
      return Array.from({ length: to - from + 1 }, (_, i) => from + i);
    }),
);

/**
 * `calls.list` as the daemon pages it (#63): `limit` records are scanned,
 * unreadable ones are left out but still count, and `nextBefore` is the oldest
 * record scanned, present whenever older records remain.
 */
function listCalls(params: unknown): Record<string, unknown> {
  const { limit = 50, before } = (params ?? {}) as {
    limit?: number;
    before?: number;
  };
  const older = allCalls.filter(
    (entry) => before === undefined || entry.globalPos < before,
  );
  const scanned = older.slice(0, limit);
  const calls = scanned.filter(
    (entry) => !unreadable.has(allCalls.indexOf(entry)),
  );
  const last = scanned.at(-1);
  return older.length > limit && last
    ? { calls, nextBefore: last.globalPos }
    : { calls };
}

function answer(method: unknown, id: unknown, params: unknown): string {
  switch (method) {
    case "version":
      return reply(id, { result: { daemonVersion: "0.1.0" } });
    case "calls.list":
      if (mode === "older") {
        break;
      }
      if (values["calls-error"]) {
        return reply(id, {
          error: { code: -32000, message: "store unavailable" },
        });
      }
      return reply(id, { result: listCalls(params) });
    case "health":
      if (mode === "no-health") {
        return reply(id, {
          error: { code: -32601, message: "Method not found" },
        });
      }
      if (mode === "unhealthy") {
        return reply(id, {
          error: { code: -32000, message: "store unavailable" },
        });
      }
      return reply(id, {
        result: {
          status: "ok",
          uptimeMs: Date.now() - reportedStartMs,
          schemaVersion: 1,
          captureContent: values.capture === "on",
          lastGlobalPosition: Number(values.events),
          erasurePending: values["erasure-pending"],
          ...(mode === "older"
            ? {}
            : {
                proxy: {
                  address: values["proxy-address"],
                  callsRecorded: allCalls.length,
                  recordsDropped: Number(values.dropped),
                },
              }),
        },
      });
  }
  return reply(id, {
    error: { code: -32601, message: "Method not found" },
  });
}

const server = http.createServer((request, response) => {
  if (mode === "hang") {
    return; // never answers
  }
  const port = (server.address() as { port: number }).port;
  if (request.headers.origin || request.headers.host !== `127.0.0.1:${port}`) {
    response.writeHead(403).end();
    return;
  }
  if (
    mode === "unauthorized" ||
    request.headers.authorization !== `Bearer ${token}`
  ) {
    response.writeHead(401, { "www-authenticate": "Bearer" }).end();
    return;
  }
  const chunks: Buffer[] = [];
  request.on("data", (chunk: Buffer) => chunks.push(chunk));
  request.on("end", () => {
    const call = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    response
      .writeHead(200, { "content-type": "application/json" })
      .end(answer(call.method, call.id, call.params));
  });
});

server.listen(0, "127.0.0.1", async () => {
  const port = (server.address() as { port: number }).port;
  if (mode === "unreachable") {
    await new Promise((resolve) => server.close(resolve));
    setInterval(() => {}, 60_000); // stay alive so the pid is
  }
  await mkdir(dataDir, { recursive: true, mode: 0o700 });
  const file = (name: string) => path.join(dataDir, name);
  await writeFile(file("control-token"), token, { mode: 0o600 });
  await writeFile(file("daemon.lock"), String(process.pid), { mode: 0o600 });
  await writeFile(
    file("daemon.json"),
    JSON.stringify({
      pid: process.pid,
      startedAtMs,
      address: `127.0.0.1:${port}`,
      schemaVersion: 1,
    }),
    { mode: 0o600 },
  );
  console.log(`fake daemon (${mode}) pid ${process.pid}, ${dataDir}`);
});
