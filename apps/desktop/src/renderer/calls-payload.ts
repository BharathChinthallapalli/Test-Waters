import type {
  CallSummary,
  CallUsage,
  Limits,
  RecentCalls,
} from "../shared/recent-calls.ts";

/**
 * Checks the recent calls that arrive over IPC before anything is drawn. The
 * main process builds them within these limits (`src/main/calls.ts`); anything
 * else, of any type or size, is refused whole, so the screen never draws a
 * shape it wasn't written for.
 */

/** Keep equal to `LIMITS` in `src/main/calls.ts` (a test checks). */
export const LIMITS: Readonly<Limits> = Object.freeze({
  pageSize: 50,
  text: 200,
  shortText: 64,
  path: 256,
  rateLimitHeaders: 32,
  headerName: 64,
  headerValue: 128,
});

const OUTCOMES: ReadonlySet<unknown> = new Set([
  "completed",
  "upstreamError",
  "clientCancelled",
  "upstreamUnreachable",
  "incomplete",
]);

/** Fields a message may have; `detail` is optional. */
const MESSAGE_MAX = 400;

type Json = Record<string, unknown>;

function isObject(value: unknown): value is Json {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

/** A string of at most `max` characters (code points). */
function isText(value: unknown, max: number): value is string {
  return (
    typeof value === "string" &&
    (value.length <= max || Array.from(value).length <= max)
  );
}

function isTextOrNull(value: unknown, max: number): boolean {
  return value === null || isText(value, max);
}

function isCountOrNull(value: unknown): boolean {
  return value === null || isCount(value);
}

function isUsage(value: unknown): value is CallUsage | null {
  return (
    value === null ||
    (isObject(value) &&
      isCount(value.inputTokens) &&
      isCount(value.outputTokens) &&
      isCountOrNull(value.cacheCreationInputTokens) &&
      isCountOrNull(value.cacheReadInputTokens))
  );
}

function isHeaders(value: unknown): boolean {
  return (
    Array.isArray(value) &&
    value.length <= LIMITS.rateLimitHeaders &&
    value.every(
      (pair) =>
        Array.isArray(pair) &&
        pair.length === 2 &&
        isText(pair[0], LIMITS.headerName) &&
        isText(pair[1], LIMITS.headerValue),
    )
  );
}

export function isCallSummary(value: unknown): value is CallSummary {
  return (
    isObject(value) &&
    isCount(value.globalPos) &&
    value.globalPos > 0 &&
    isText(value.runId, LIMITS.text) &&
    isText(value.method, LIMITS.shortText) &&
    isText(value.path, LIMITS.path) &&
    isCount(value.status) &&
    value.status <= 999 &&
    OUTCOMES.has(value.outcome) &&
    typeof value.streamed === "boolean" &&
    isTextOrNull(value.model, LIMITS.text) &&
    isTextOrNull(value.requestId, LIMITS.text) &&
    isTextOrNull(value.stopReason, LIMITS.shortText) &&
    isTextOrNull(value.errorType, LIMITS.shortText) &&
    isUsage(value.usage) &&
    isCount(value.startedAtMs) &&
    isCountOrNull(value.ttfbMs) &&
    isCount(value.durationMs) &&
    isHeaders(value.rateLimitHeaders) &&
    isText(value.traceId, LIMITS.shortText) &&
    isTextOrNull(value.userAgent, LIMITS.text)
  );
}

/**
 * The value when it is a {@link RecentCalls} within the limits, else null. A
 * page must be newest first (strictly falling positions), and its `nextBefore`
 * no newer than its oldest call.
 */
export function parseRecentCalls(value: unknown): RecentCalls | null {
  if (!isObject(value)) {
    return null;
  }
  switch (value.state) {
    case "unsupported":
      return { state: "unsupported" };
    case "failed":
      if (
        !isText(value.message, MESSAGE_MAX) ||
        !(value.detail === undefined || isText(value.detail, MESSAGE_MAX))
      ) {
        return null;
      }
      return value.detail === undefined
        ? { state: "failed", message: value.message }
        : { state: "failed", message: value.message, detail: value.detail };
    case "loaded": {
      const { calls, nextBefore } = value;
      if (
        !Array.isArray(calls) ||
        calls.length > LIMITS.pageSize ||
        !calls.every(isCallSummary) ||
        !(nextBefore === null || (isCount(nextBefore) && nextBefore > 0))
      ) {
        return null;
      }
      for (let i = 1; i < calls.length; i++) {
        if (calls[i].globalPos >= calls[i - 1].globalPos) {
          return null;
        }
      }
      const oldest = calls.at(-1)?.globalPos;
      if (nextBefore !== null && oldest !== undefined && nextBefore > oldest) {
        return null;
      }
      return { state: "loaded", calls, nextBefore };
    }
    default:
      return null;
  }
}
