import assert from "node:assert/strict";
import { test } from "node:test";
import {
  parseRecentCalls,
  LIMITS as RENDERER_LIMITS,
} from "../renderer/calls-payload.ts";
import { clip, LIMITS, parseBefore, parseCallsList } from "./calls.ts";

function record(overrides: Record<string, unknown> = {}) {
  return {
    provider: "anthropic",
    method: "POST",
    path: "/v1/messages",
    status: 200,
    outcome: "completed",
    streamed: true,
    model: "claude-opus-4-1",
    requestId: "req_011CZ",
    stopReason: "end_turn",
    usage: {
      inputTokens: 12,
      outputTokens: 340,
      cacheCreationInputTokens: 0,
      cacheReadInputTokens: 9000,
    },
    startedAtMs: 1_790_000_000_000,
    ttfbMs: 420,
    durationMs: 3200,
    requestBytes: 1000,
    responseBytes: 4000,
    rateLimitHeaders: {
      "retry-after": "12",
      "anthropic-ratelimit-requests-remaining": "49",
    },
    traceId: "0af7651916cd43dd8448eb211c80319c",
    userAgent: "claude-cli/2.1.0 (external, cli)",
    contentTruncated: false,
    ...overrides,
  };
}

function page(calls: unknown[], nextBefore?: number | null) {
  return nextBefore === undefined ? { calls } : { calls, nextBefore };
}

function entry(globalPos: number, call: unknown = record()) {
  return { globalPos, runId: "cc-7d2f", call };
}

test("main and renderer apply the same limits", () => {
  assert.deepEqual(LIMITS, RENDERER_LIMITS);
});

test("an entry is flattened; byte counts and the provider aren't passed on", () => {
  const result = parseCallsList(page([entry(9)], 9));
  assert.deepEqual(result, {
    state: "loaded",
    nextBefore: 9,
    calls: [
      {
        globalPos: 9,
        runId: "cc-7d2f",
        method: "POST",
        path: "/v1/messages",
        status: 200,
        outcome: "completed",
        streamed: true,
        model: "claude-opus-4-1",
        requestId: "req_011CZ",
        stopReason: "end_turn",
        errorType: null,
        usage: {
          inputTokens: 12,
          outputTokens: 340,
          cacheCreationInputTokens: 0,
          cacheReadInputTokens: 9000,
        },
        startedAtMs: 1_790_000_000_000,
        ttfbMs: 420,
        durationMs: 3200,
        // retry-after first, then the rest by name.
        rateLimitHeaders: [
          ["retry-after", "12"],
          ["anthropic-ratelimit-requests-remaining", "49"],
        ],
        traceId: "0af7651916cd43dd8448eb211c80319c",
        userAgent: "claude-cli/2.1.0 (external, cli)",
      },
    ],
  });
});

test("absent options become null; usage absent stays null, never zero", () => {
  const minimal = record();
  for (const key of [
    "model",
    "requestId",
    "stopReason",
    "usage",
    "ttfbMs",
    "userAgent",
  ]) {
    delete (minimal as Record<string, unknown>)[key];
  }
  const result = parseCallsList(page([entry(1, minimal)]));
  assert.ok(result);
  const [call] = result.calls;
  assert.equal(call?.usage, null);
  assert.equal(call?.model, null);
  assert.equal(call?.ttfbMs, null);
  assert.equal(result.nextBefore, null);
  // Absent cache counts are null, not 0.
  const noCache = parseCallsList(
    page([entry(1, record({ usage: { inputTokens: 1, outputTokens: 2 } }))]),
  );
  assert.deepEqual(noCache?.calls[0]?.usage, {
    inputTokens: 1,
    outputTokens: 2,
    cacheCreationInputTokens: null,
    cacheReadInputTokens: null,
  });
});

test("long strings are cut, control characters replaced, headers capped", () => {
  const headers = Object.fromEntries(
    Array.from({ length: 40 }, (_, i) => [
      `anthropic-ratelimit-${String(i).padStart(2, "0")}-${"n".repeat(80)}`,
      "v".repeat(500),
    ]),
  );
  const result = parseCallsList(
    page([
      entry(
        1,
        record({
          model: "m".repeat(1000),
          stopReason: "a\u0000b\nc",
          rateLimitHeaders: headers,
        }),
      ),
    ]),
  );
  const call = result?.calls[0];
  assert.ok(call);
  assert.equal(Array.from(call.model ?? "").length, LIMITS.text);
  assert.ok(call.model?.endsWith("…"));
  assert.equal(call.stopReason, "a�b�c");
  assert.equal(call.rateLimitHeaders.length, LIMITS.rateLimitHeaders);
  for (const [name, value] of call.rateLimitHeaders) {
    assert.equal(name.length, LIMITS.headerName);
    assert.equal(value.length, LIMITS.headerValue);
  }
  assert.equal(clip("short", 10), "short");
  // Cut by code point, so a surrogate pair is never split.
  assert.equal(clip("😀😀😀", 2), "😀…");
});

test("bidi embeddings, overrides and isolates are neutralised", () => {
  // "Trojan Source" controls: U+202A to U+202E and U+2066 to U+2069.
  const bidi = [
    0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069,
  ].map((code) => String.fromCodePoint(code));
  for (const control of bidi) {
    assert.equal(
      clip(`a${control}b`, 10),
      "a�b",
      control.codePointAt(0)?.toString(16),
    );
  }
  // Neighbours that only mark direction, or join text, are left alone.
  for (const kept of ["‎", "‏", "⁥", "⁪", " "]) {
    assert.equal(clip(`a${kept}b`, 10), `a${kept}b`);
  }
  const call = parseCallsList(
    page([entry(1, record({ model: "claude‮gnp.exe" }))]),
  )?.calls[0];
  assert.equal(call?.model, "claude�gnp.exe");
});

test("the header cap keeps retry-after, x-should-retry and request-id first", () => {
  const many = Object.fromEntries(
    Array.from({ length: 40 }, (_, i) => [
      `anthropic-ratelimit-unified-${String(i).padStart(2, "0")}`,
      "v",
    ]),
  );
  const call = parseCallsList(
    page([
      entry(
        1,
        record({
          rateLimitHeaders: {
            ...many,
            "x-should-retry": "true",
            "request-id": "req_1",
            "retry-after": "17",
          },
        }),
      ),
    ]),
  )?.calls[0];
  assert.ok(call);
  const names = call.rateLimitHeaders.map(([name]) => name);
  assert.equal(names.length, LIMITS.rateLimitHeaders);
  assert.deepEqual(names.slice(0, 4), [
    "retry-after",
    "x-should-retry",
    "request-id",
    "anthropic-ratelimit-unified-00",
  ]);
});

test("what the main process builds always passes the renderer's check", () => {
  const huge = "x".repeat(5000);
  const worst = record({
    method: huge,
    path: huge,
    model: huge,
    requestId: huge,
    stopReason: huge,
    errorType: huge,
    traceId: huge,
    userAgent: huge,
    rateLimitHeaders: Object.fromEntries(
      Array.from({ length: 100 }, (_, i) => [`${i}${huge}`, huge]),
    ),
  });
  const result = parseCallsList(
    page(
      Array.from({ length: 50 }, (_, i) => ({
        globalPos: 1000 - i,
        runId: huge,
        call: worst,
      })),
      951,
    ),
  );
  assert.ok(result);
  assert.deepEqual(parseRecentCalls(result), result);
  // The bound on what crosses IPC every three seconds.
  assert.ok(JSON.stringify(result).length < 450_000);
});

test("anything but a well-formed page is refused whole", () => {
  const bad: unknown[] = [
    null,
    [],
    { calls: "none" },
    page(Array.from({ length: 51 }, (_, i) => entry(100 - i))),
    page([entry(0)]),
    page([entry(-1)]),
    page([{ globalPos: 1, runId: 7, call: record() }]),
    page([{ globalPos: 1, runId: "r" }]),
    page([entry(1, record({ status: "200" }))]),
    page([entry(1, record({ status: 1000 }))]),
    page([entry(1, record({ outcome: "exploded" }))]),
    page([entry(1, record({ streamed: "yes" }))]),
    page([entry(1, record({ usage: { inputTokens: -1, outputTokens: 0 } }))]),
    page([entry(1, record({ usage: { inputTokens: 1.5, outputTokens: 0 } }))]),
    page([entry(1, record({ durationMs: null }))]),
    page([entry(1, record({ rateLimitHeaders: { "retry-after": 12 } }))]),
    page([entry(1, record({ rateLimitHeaders: null }))]),
    page([entry(1, record({ traceId: undefined }))]),
    // Not newest first, a repeated position, a cursor newer than the page.
    page([entry(1), entry(2)]),
    page([entry(2), entry(2)]),
    page([entry(5)], 6),
    page([entry(5)], 0),
  ];
  for (const value of bad) {
    assert.equal(parseCallsList(value), null, JSON.stringify(value));
  }
  assert.deepEqual(parseCallsList(page([])), {
    state: "loaded",
    calls: [],
    nextBefore: null,
  });
});

test("nextBefore is the oldest record scanned: below the calls, or on an empty page", () => {
  // Unreadable records count towards the page (calls.list, #63).
  assert.deepEqual(parseCallsList(page([], 40)), {
    state: "loaded",
    calls: [],
    nextBefore: 40,
  });
  assert.equal(parseCallsList(page([entry(9)], 4))?.nextBefore, 4);
  const empty = parseCallsList(page([], 40));
  assert.deepEqual(parseRecentCalls(empty), empty);
});

test("a Load older cursor is a positive safe integer", () => {
  assert.equal(parseBefore(51), 51);
  for (const value of [0, -1, 1.5, "51", null, undefined, 2 ** 53, NaN]) {
    assert.equal(parseBefore(value), null, String(value));
  }
});
