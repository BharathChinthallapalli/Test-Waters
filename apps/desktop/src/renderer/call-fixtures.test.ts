// Fixtures for the calls tests. Named *.test.ts so the build leaves it out;
// it has no tests of its own.
import type { DaemonStatus, RunningStatus } from "../shared/daemon-status.ts";
import type { CallSummary, RecentCalls } from "../shared/recent-calls.ts";

/** A completed, streamed call with cache reads, as the main process sends it. */
export function summary(
  globalPos: number,
  overrides: Partial<CallSummary> = {},
): CallSummary {
  return {
    globalPos,
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
      inputTokens: 408,
      outputTokens: 812,
      cacheCreationInputTokens: 0,
      cacheReadInputTokens: 12_000,
    },
    startedAtMs: 1_790_000_000_000,
    ttfbMs: 420,
    durationMs: 3200,
    rateLimitHeaders: [["anthropic-ratelimit-requests-remaining", "49"]],
    traceId: "0af7651916cd43dd8448eb211c80319c",
    userAgent: null,
    ...overrides,
  };
}

/** Calls at these positions, newest first. */
export function positions(...list: number[]): CallSummary[] {
  return list.map((pos) => summary(pos));
}

export const PROXY = {
  address: "127.0.0.1:4101",
  callsRecorded: 3,
  recordsDropped: 0,
};

/** A running feature 03 daemon with a proxy, carrying `calls`. */
export function running(
  calls: RecentCalls,
  overrides: Partial<RunningStatus> = {},
): RunningStatus {
  return {
    state: "running",
    message: "The daemon is running and answering on 127.0.0.1:4100.",
    checkedAtMs: 1,
    customDataDir: false,
    platform: "posix",
    dataDir: "/home/ann/.local/share/callsheet",
    address: "127.0.0.1:4100",
    pid: 7,
    daemonVersion: "0.2.0",
    schemaVersion: 3,
    uptimeMs: 60_000,
    health: {
      captureContent: false,
      lastGlobalPosition: 99,
      erasurePending: false,
      proxy: PROXY,
    },
    calls,
    ...overrides,
  };
}

export const NOT_RUNNING: DaemonStatus = {
  state: "not-running",
  message: "No Callsheet daemon is running for this user.",
  checkedAtMs: 1,
  customDataDir: false,
  platform: "posix",
  dataDir: "/home/ann/.local/share/callsheet",
  stale: false,
};

export function page(
  calls: CallSummary[],
  nextBefore: number | null = null,
): RecentCalls {
  return { state: "loaded", calls, nextBefore };
}
