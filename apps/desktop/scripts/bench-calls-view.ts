// Times what the renderer computes for the Recent calls list on each refresh:
// `applyStatus` and `describeCalls` over the most calls it holds (1,000),
// the work N3 of PR #69's review measured at about 300 ms. Not shipped.
//
//   node scripts/bench-calls-view.ts [--rounds <n>] [--step-ms <ms>]
//
// Prints the median and the slowest round, in milliseconds. By default each
// round is a refresh a minute after the last, when every row's relative time
// changes; --step-ms 3000 is the app's usual poll within a minute.
import { parseArgs } from "node:util";
import {
  applyStatus,
  type CallsList,
  EMPTY_LIST,
  MAX_HELD_CALLS,
} from "../src/renderer/calls-state.ts";
import { describeCalls } from "../src/renderer/calls-view.ts";
import type { RunningStatus } from "../src/shared/daemon-status.ts";
import type { CallSummary } from "../src/shared/recent-calls.ts";

const { values } = parseArgs({
  options: {
    rounds: { type: "string", default: "40" },
    "step-ms": { type: "string", default: "60000" },
  },
});

const NOW = Date.UTC(2026, 8, 27, 14, 0, 0);

function call(i: number): CallSummary {
  return {
    globalPos: 100_000 - i,
    runId: "cc-5c1f0d2e",
    method: "POST",
    path: "/v1/messages",
    status: 200,
    outcome: "completed",
    streamed: true,
    model: "claude-sonnet-4-5-20250929",
    requestId: `req_${i}`,
    stopReason: "end_turn",
    errorType: null,
    usage: {
      inputTokens: 3 + i,
      outputTokens: 180 + i,
      cacheCreationInputTokens: 2_314,
      cacheReadInputTokens: 48_210 + i,
    },
    startedAtMs: NOW - i * 7 * 60_000,
    ttfbMs: 420,
    durationMs: 3_200 + i * 97,
    rateLimitHeaders: [["anthropic-ratelimit-unified-status", "allowed"]],
    traceId: "0af7651916cd43dd8448eb211c80319c",
    userAgent: "claude-cli/2.1.227 (external, cli)",
  };
}

const calls = Array.from({ length: MAX_HELD_CALLS }, (_, i) => call(i));
const status: RunningStatus = {
  state: "running",
  message: "",
  checkedAtMs: 1,
  customDataDir: false,
  platform: "posix",
  dataDir: "/tmp/x",
  address: "127.0.0.1:4100",
  pid: 1,
  daemonVersion: "0.1.0",
  schemaVersion: 1,
  uptimeMs: 1,
  health: {
    captureContent: false,
    lastGlobalPosition: 100_000,
    erasurePending: false,
    proxy: {
      address: "127.0.0.1:4101",
      callsRecorded: 1000,
      recordsDropped: 0,
    },
  },
  // The newest page, as every status carries it.
  calls: {
    state: "loaded",
    calls: calls.slice(0, 50),
    nextBefore: calls[49].globalPos,
  },
} as RunningStatus;

const ui = { loadingOlder: false, olderError: null };
const times: number[] = [];
// What the screen holds after "Load older" up to the limit.
let list: CallsList = {
  ...EMPTY_LIST,
  run: `${status.address}|${status.pid}`,
  calls,
  nextBefore: null,
  newestFrom: calls[49].globalPos,
  loaded: true,
};
for (let round = 0; round < Number(values.rounds); round += 1) {
  const started = performance.now();
  list = applyStatus(list, status);
  const view = describeCalls(
    status,
    list,
    ui,
    NOW + round * Number(values["step-ms"]),
    "en-US",
  );
  times.push(performance.now() - started);
  if (view?.rows.length !== MAX_HELD_CALLS) {
    throw new Error("wrong row count");
  }
}
times.sort((a, b) => a - b);
const median = times[Math.floor(times.length / 2)];
console.log(
  `${MAX_HELD_CALLS} rows, ${times.length} refreshes: median ${median.toFixed(1)} ms, slowest ${times.at(-1)?.toFixed(1)} ms`,
);
