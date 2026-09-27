/**
 * The recent model calls the main process passes to the renderer (feature 03,
 * requirement 7). Built from `calls.list` by `src/main/calls.ts`, checked again
 * by `src/renderer/calls-payload.ts` before anything is drawn. Types only: the
 * sandboxed renderer can't load shared runtime code, so each side keeps its own
 * copy of the limits below and a test keeps them equal.
 *
 * A {@link CallSummary} is one `calls.list` entry flattened, with every string
 * cut to a known length and at most {@link Limits.rateLimitHeaders} headers, so
 * a page has a fixed upper size however large the daemon's reply was.
 */

import type { CallOutcome } from "@callsheet/api-types";

export type { CallOutcome };

/** The limits both sides apply; the values live in each side's module. */
export interface Limits {
  /** Calls per page, and the page size asked for. */
  pageSize: number;
  /** Characters kept of model, request id, run id and user agent. */
  text: number;
  /** Characters kept of stop reason, error type and method. */
  shortText: number;
  /** Characters kept of the path. */
  path: number;
  rateLimitHeaders: number;
  headerName: number;
  headerValue: number;
}

export interface CallUsage {
  inputTokens: number;
  outputTokens: number;
  cacheCreationInputTokens: number | null;
  cacheReadInputTokens: number | null;
}

export interface CallSummary {
  /** The event's global position: a stable key, and the paging cursor. */
  globalPos: number;
  runId: string;
  method: string;
  path: string;
  status: number;
  outcome: CallOutcome;
  streamed: boolean;
  model: string | null;
  requestId: string | null;
  stopReason: string | null;
  errorType: string | null;
  /** Null when the provider reported none; never a guess. */
  usage: CallUsage | null;
  startedAtMs: number;
  ttfbMs: number | null;
  durationMs: number;
  /** Recorded response headers as `[name, value]`, sorted by name. */
  rateLimitHeaders: Array<[string, string]>;
  traceId: string;
  userAgent: string | null;
}

/** One page of `calls.list`, newest first. */
export interface CallsPage {
  state: "loaded";
  calls: CallSummary[];
  /**
   * Pass as `before` for the next older page; null only on the last page. It
   * is the oldest record the daemon scanned, and records it couldn't read count
   * towards the page, so an empty page can still have one.
   */
  nextBefore: number | null;
}

/** The daemon predates `calls.list` (JSON-RPC -32601). */
export interface CallsUnsupported {
  state: "unsupported";
}

/** Asking for calls failed; `message` is one plain sentence. */
export interface CallsFailed {
  state: "failed";
  message: string;
  detail?: string;
}

export type RecentCalls = CallsPage | CallsUnsupported | CallsFailed;
