import assert from "node:assert/strict";
import { test } from "node:test";
import { summary } from "./call-fixtures.test.ts";
import { LIMITS, parseRecentCalls } from "./calls-payload.ts";

const loaded = (calls: unknown, nextBefore: unknown = null) => ({
  state: "loaded",
  calls,
  nextBefore,
});

test("well-formed payloads pass unchanged", () => {
  const page = loaded([summary(9), summary(4)], 4);
  assert.deepEqual(parseRecentCalls(page), page);
  assert.deepEqual(parseRecentCalls(loaded([])), loaded([]));
  assert.deepEqual(parseRecentCalls({ state: "unsupported" }), {
    state: "unsupported",
  });
  assert.deepEqual(
    parseRecentCalls({ state: "failed", message: "No.", detail: "E" }),
    { state: "failed", message: "No.", detail: "E" },
  );
  assert.deepEqual(parseRecentCalls({ state: "failed", message: "No." }), {
    state: "failed",
    message: "No.",
  });
});

test("unknown members are dropped from what is drawn", () => {
  const parsed = parseRecentCalls({ state: "unsupported", html: "<b>" });
  assert.deepEqual(parsed, { state: "unsupported" });
});

test("wrong types, sizes and orders are refused whole", () => {
  const tooLong = "x".repeat(LIMITS.text + 1);
  const bad: unknown[] = [
    null,
    "loaded",
    { state: "exploded" },
    { state: "failed" },
    { state: "failed", message: 7 },
    { state: "failed", message: "x".repeat(401) },
    loaded("calls"),
    loaded([summary(1)], 0),
    loaded([summary(1)], "1"),
    loaded([summary(5)], 6), // a cursor newer than the page
    loaded(Array.from({ length: 51 }, (_, i) => summary(100 - i))),
    loaded([summary(1), summary(2)]), // oldest first
    loaded([summary(2), summary(2)]),
    loaded([summary(0)]),
    loaded([summary(1, { model: tooLong })]),
    loaded([summary(1, { runId: tooLong })]),
    loaded([summary(1, { path: "/".repeat(LIMITS.path + 1) })]),
    loaded([summary(1, { outcome: "exploded" as "completed" })]),
    loaded([summary(1, { status: 1000 })]),
    loaded([summary(1, { streamed: 1 as unknown as boolean })]),
    loaded([summary(1, { durationMs: -1 })]),
    loaded([summary(1, { startedAtMs: 1.5 })]),
    loaded([
      summary(1, {
        usage: {
          inputTokens: 1,
          outputTokens: Number.NaN,
          cacheCreationInputTokens: null,
          cacheReadInputTokens: null,
        },
      }),
    ]),
    loaded([summary(1, { usage: undefined as unknown as null })]),
    loaded([
      summary(1, {
        rateLimitHeaders: [["a"]] as unknown as [string, string][],
      }),
    ]),
    loaded([
      summary(1, {
        rateLimitHeaders: Array.from(
          { length: LIMITS.rateLimitHeaders + 1 },
          (_, i): [string, string] => [`h${i}`, "v"],
        ),
      }),
    ]),
    loaded([
      summary(1, {
        rateLimitHeaders: [["h", "v".repeat(LIMITS.headerValue + 1)]],
      }),
    ]),
    loaded([summary(1, { traceId: null as unknown as string })]),
  ];
  for (const value of bad) {
    assert.equal(parseRecentCalls(value), null, JSON.stringify(value));
  }
});

test("lengths are counted in characters, not UTF-16 units", () => {
  const emoji = "😀".repeat(LIMITS.text);
  assert.ok(parseRecentCalls(loaded([summary(1, { model: emoji })])));
});
