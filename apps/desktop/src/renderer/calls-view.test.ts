import assert from "node:assert/strict";
import { test } from "node:test";
import type { CallSummary } from "../shared/recent-calls.ts";
import {
  NOT_RUNNING,
  PROXY,
  page,
  positions,
  running,
  summary,
} from "./call-fixtures.test.ts";
import { applyOlder, applyStatus, EMPTY_LIST } from "./calls-state.ts";
import {
  type CallsUi,
  type CallsView,
  describeCall,
  describeCalls,
  describeConnect,
  describeOutcome,
  EMPTY_TEXT,
  LOGIN_NOTE,
} from "./calls-view.ts";

const IDLE: CallsUi = { loadingOlder: false, olderError: null };
const NOW = 1_790_000_060_000; // a minute after the fixture's calls

function view(
  status: Parameters<typeof applyStatus>[1],
  ui: CallsUi = IDLE,
): CallsView | null {
  return describeCalls(
    status,
    applyStatus(EMPTY_LIST, status),
    ui,
    NOW,
    "en-US",
  );
}

test("connect: the export line, and that the login keeps working", () => {
  const connect = describeConnect(running(page([])));
  assert.deepEqual(connect, {
    kind: "connect",
    text: "Run this in the terminal you start Claude Code from:",
    commands: [
      { shell: null, text: "export ANTHROPIC_BASE_URL=http://127.0.0.1:4101" },
    ],
    after: LOGIN_NOTE,
  });
  const windows = describeConnect(running(page([]), { platform: "windows" }));
  assert.ok(windows?.kind === "connect");
  assert.deepEqual(
    windows.commands.map((command) => command.shell),
    ["PowerShell", "Command Prompt"],
  );
  assert.match(windows.text, /one of these/);
});

test("connect: nothing while the daemon isn't running", () => {
  assert.equal(describeConnect(NOT_RUNNING), null);
  assert.equal(view(NOT_RUNNING), null);
});

test("an older daemon without the proxy: said plainly, and no list", () => {
  const withoutProxy = running({ state: "unsupported" });
  if (withoutProxy.health) {
    withoutProxy.health = { ...withoutProxy.health, proxy: null };
  }
  const connect = describeConnect(withoutProxy);
  assert.equal(connect?.kind, "unavailable");
  assert.ok(connect?.kind === "unavailable");
  assert.equal(connect.title, "Call recording isn't available");
  assert.match(connect.text, /newer cs-daemon/);
  assert.equal(view(withoutProxy), null);
  // Without health at all, the same.
  const noHealth = running({ state: "unsupported" }, { health: null });
  assert.equal(describeConnect(noHealth)?.kind, "unavailable");
  assert.equal(view(noHealth), null);
});

test("a proxy address the app won't use gets no command", () => {
  const odd = running(page([]));
  if (odd.health) {
    odd.health = {
      ...odd.health,
      proxy: { ...PROXY, address: "10.0.0.1:4101" },
    };
  }
  const connect = describeConnect(odd);
  assert.equal(connect?.kind, "unavailable");
});

test("empty: says how to get calls here", () => {
  const empty = view(running(page([])));
  assert.equal(empty?.state, "empty");
  assert.equal(
    empty?.message,
    "No calls yet. Point Claude Code at the address above and they appear here.",
  );
  assert.equal(EMPTY_TEXT, empty?.message);
  assert.deepEqual(empty?.rows, []);
  assert.equal(empty?.loadOlder, "hidden");
});

test("an empty page with nextBefore offers Load older, never 'no calls'", () => {
  const shown = view(running(page([], 60)));
  assert.equal(shown?.state, "empty");
  assert.equal(shown?.loadOlder, "ready");
  assert.notEqual(shown?.message, EMPTY_TEXT);
  assert.match(shown?.message ?? "", /Older ones may still be listed/);
  // An empty older page keeps the button too.
  const status = running(page(positions(90), 60));
  const after = applyOlder(applyStatus(EMPTY_LIST, status), 60, page([], 40));
  const list = describeCalls(status, after.list, IDLE, NOW, "en-US");
  assert.equal(list?.state, "list");
  assert.equal(list?.loadOlder, "ready");
});

test("error: what happened, that it retries, and the code only as detail", () => {
  const error = view(
    running({
      state: "failed",
      message: "The daemon didn't return its recent calls.",
      detail: "Error -32000: store unavailable",
    }),
  );
  assert.equal(error?.state, "error");
  assert.equal(
    error?.message,
    "The daemon didn't return its recent calls. The app tries again every 3 seconds.",
  );
  assert.equal(error?.detail, "Error -32000: store unavailable");
  assert.doesNotMatch(error?.message ?? "", /-\d{3,}|JSON-RPC/);

  const unsupported = view(running({ state: "unsupported" }));
  assert.equal(unsupported?.state, "error");
  assert.match(unsupported?.message ?? "", /newer cs-daemon/);
});

test("populated: one row per call, newest first, with Load older", () => {
  const list = view(running(page(positions(9, 7, 4), 4)));
  assert.equal(list?.state, "list");
  assert.deepEqual(
    list?.rows.map((row) => row.key),
    ["9", "7", "4"],
  );
  assert.equal(list?.loadOlder, "ready");
  assert.equal(
    view(running(page(positions(9, 7, 4), 4)), {
      loadingOlder: true,
      olderError: null,
    })?.loadOlder,
    "loading",
  );
  assert.equal(view(running(page(positions(9))))?.loadOlder, "hidden");
});

test("populated after a failed refresh: the list stays, with a note", () => {
  const status = running(page(positions(9, 7)));
  let list = applyStatus(EMPTY_LIST, status);
  list = applyStatus(
    list,
    running({
      state: "failed",
      message: "The daemon didn't return its recent calls.",
    }),
  );
  const shown = describeCalls(status, list, IDLE, NOW, "en-US");
  assert.equal(shown?.state, "list");
  assert.equal(shown?.rows.length, 2);
  assert.match(shown?.note ?? "", /^Couldn't refresh the list: /);
});

test("a Load older failure is shown by the button", () => {
  const status = running(page(positions(9, 7), 7));
  const failed = applyOlder(applyStatus(EMPTY_LIST, status), 7, {
    state: "failed",
    message: "The daemon didn't return its recent calls.",
  });
  const shown = describeCalls(
    status,
    failed.list,
    { loadingOlder: false, olderError: failed.error },
    NOW,
    "en-US",
  );
  assert.equal(shown?.olderError, "The daemon didn't return its recent calls.");
});

test("calls the proxy couldn't record are counted", () => {
  const status = running(page([]));
  if (status.health?.proxy) {
    status.health.proxy = { ...status.health.proxy, recordsDropped: 3 };
  }
  assert.equal(
    view(status)?.dropped,
    "3 calls since the daemon started went through but weren't recorded.",
  );
  assert.equal(view(running(page([])))?.dropped, null);
});

test("outcomes are text with a tone; errors name the status", () => {
  const cases: Array<[Partial<CallSummary>, string, string]> = [
    [{ outcome: "completed" }, "Completed", "ok"],
    [{ outcome: "upstreamError", status: 429 }, "Error 429", "danger"],
    [{ outcome: "upstreamError", status: 200 }, "Stream error", "danger"],
    [{ outcome: "clientCancelled" }, "Cancelled", "off"],
    [{ outcome: "upstreamUnreachable", status: 502 }, "Unreachable", "danger"],
    [{ outcome: "incomplete" }, "Incomplete", "warning"],
  ];
  for (const [overrides, label, tone] of cases) {
    assert.deepEqual(describeOutcome(summary(1, overrides)), { label, tone });
  }
});

test("a row: input includes the cache, with the breakdown on hover", () => {
  const row = describeCall(summary(9), NOW, "en-US");
  assert.equal(row.tokensIn, "12,408");
  assert.equal(
    row.tokensInTitle,
    "12,408 input tokens: 408 uncached, 12,000 read from cache, 0 written to cache",
  );
  assert.equal(row.tokensOut, "812");
  assert.equal(row.duration, "3.2 s");
  assert.equal(row.time, "1 min ago");
  assert.match(row.timeTitle, /2026/);
  assert.equal(row.path, null); // /v1/messages isn't repeated on every row
  assert.equal(row.streamed, true);
  assert.equal(
    row.label,
    "1 min ago, claude-opus-4-1, streamed, Completed, 12,408 tokens in, 812 tokens out, 3.2 s",
  );
});

test("a row without usage shows a dash, never 0", () => {
  const row = describeCall(
    summary(3, {
      path: "/v1/messages/count_tokens",
      model: null,
      usage: null,
      streamed: false,
    }),
    NOW,
    "en-US",
  );
  assert.equal(row.tokensIn, "—");
  assert.equal(row.tokensOut, "—");
  assert.equal(row.tokensInTitle, null);
  assert.equal(row.path, "/v1/messages/count_tokens");
  assert.equal(row.model, null);
  assert.match(
    row.label,
    /Model not reported, \/v1\/messages\/count_tokens, Completed, no token counts/,
  );
  const tokens = row.details.find((detail) => detail.key === "tokens");
  assert.equal(tokens?.value, "Not reported by the provider.");
});

test("details: ids, reasons, and the rate-limit headers without request-id", () => {
  const row = describeCall(
    summary(5, {
      outcome: "upstreamError",
      status: 429,
      errorType: "rate_limit_error",
      stopReason: null,
      requestId: "req_018EeWyXxfu5pfWkrYcMdjWG",
      rateLimitHeaders: [
        ["anthropic-ratelimit-requests-remaining", "0"],
        ["request-id", "req_018EeWyXxfu5pfWkrYcMdjWG"],
        ["retry-after", "17"],
      ],
    }),
    NOW,
    "en-US",
  );
  const detail = (key: string) =>
    row.details.find((entry) => entry.key === key)?.value;
  assert.equal(detail("request-id"), "req_018EeWyXxfu5pfWkrYcMdjWG");
  assert.equal(detail("error-type"), "rate_limit_error");
  assert.equal(detail("stop-reason"), undefined);
  assert.equal(detail("run"), "cc-7d2f");
  assert.equal(detail("trace"), "0af7651916cd43dd8448eb211c80319c");
  assert.equal(detail("status"), "429");
  assert.equal(detail("request"), "POST /v1/messages");
  assert.deepEqual(row.headers, [
    ["anthropic-ratelimit-requests-remaining", "0"],
    ["retry-after", "17"],
  ]);
  const plain = describeCall(summary(6, { requestId: null }), NOW, "en-US");
  assert.equal(
    plain.details.find((entry) => entry.key === "request-id")?.value,
    "Not reported",
  );
});
