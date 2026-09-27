import type {
  CallOutcome,
  CallSummary,
  CallsPage,
  CallUsage,
  Limits,
} from "../shared/recent-calls.ts";

/**
 * Reads a `calls.list` result (`cs_core::llm::CallsListResult`) into the
 * bounded {@link CallsPage} the renderer gets. Wrong types refuse the whole
 * page; long strings are cut (ending in "…") and extra rate-limit headers are
 * dropped, so a page's size has a fixed upper bound. Byte counts, the provider
 * and `contentTruncated` aren't shown, so they aren't passed on.
 */

/** Keep equal to `LIMITS` in `src/renderer/calls-payload.ts` (a test checks). */
export const LIMITS: Readonly<Limits> = Object.freeze({
  pageSize: 50,
  text: 200,
  shortText: 64,
  path: 256,
  rateLimitHeaders: 32,
  headerName: 64,
  headerValue: 128,
});

const OUTCOMES: ReadonlySet<string> = new Set<CallOutcome>([
  "completed",
  "upstreamError",
  "clientCancelled",
  "upstreamUnreachable",
  "incomplete",
]);

/** C0 and C1 controls, which a record never needs and a screen can't show. */
function isControl(char: string): boolean {
  const code = char.codePointAt(0) ?? 0;
  return code <= 0x1f || (code >= 0x7f && code <= 0x9f);
}

/**
 * The value cut to `max` characters (code points, so a surrogate pair is never
 * split), the last being "…" when it was longer; controls become U+FFFD.
 */
export function clip(value: string, max: number): string {
  const chars = Array.from(value, (char) => (isControl(char) ? "�" : char));
  return chars.length <= max
    ? chars.join("")
    : `${chars.slice(0, max - 1).join("")}…`;
}

function isCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

type Json = Record<string, unknown>;

function isObject(value: unknown): value is Json {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Thrown inside the parser; {@link parseCallsList} turns it into null. */
class Malformed extends Error {}

function text(record: Json, key: string, max: number): string {
  const value = record[key];
  if (typeof value !== "string") {
    throw new Malformed(key);
  }
  return clip(value, max);
}

function optionalText(record: Json, key: string, max: number): string | null {
  return record[key] === undefined || record[key] === null
    ? null
    : text(record, key, max);
}

function count(record: Json, key: string): number {
  const value = record[key];
  if (!isCount(value)) {
    throw new Malformed(key);
  }
  return value;
}

function optionalCount(record: Json, key: string): number | null {
  return record[key] === undefined || record[key] === null
    ? null
    : count(record, key);
}

function usage(value: unknown): CallUsage | null {
  if (value === undefined || value === null) {
    return null;
  }
  if (!isObject(value)) {
    throw new Malformed("usage");
  }
  return {
    inputTokens: count(value, "inputTokens"),
    outputTokens: count(value, "outputTokens"),
    cacheCreationInputTokens: optionalCount(value, "cacheCreationInputTokens"),
    cacheReadInputTokens: optionalCount(value, "cacheReadInputTokens"),
  };
}

function headers(value: unknown): Array<[string, string]> {
  if (!isObject(value)) {
    throw new Malformed("rateLimitHeaders");
  }
  const entries: Array<[string, string]> = [];
  for (const [name, headerValue] of Object.entries(value)) {
    if (typeof headerValue !== "string") {
      throw new Malformed("rateLimitHeaders");
    }
    entries.push([name, headerValue]);
  }
  return entries
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .slice(0, LIMITS.rateLimitHeaders)
    .map(([name, headerValue]) => [
      clip(name, LIMITS.headerName),
      clip(headerValue, LIMITS.headerValue),
    ]);
}

function entry(value: unknown): CallSummary {
  if (!isObject(value) || !isObject(value.call)) {
    throw new Malformed("entry");
  }
  const call = value.call;
  const status = count(call, "status");
  const outcome = call.outcome;
  if (
    status > 999 ||
    typeof outcome !== "string" ||
    !OUTCOMES.has(outcome) ||
    typeof call.streamed !== "boolean"
  ) {
    throw new Malformed("call");
  }
  const globalPos = count(value, "globalPos");
  if (globalPos === 0) {
    throw new Malformed("globalPos");
  }
  return {
    globalPos,
    runId: text(value, "runId", LIMITS.text),
    method: text(call, "method", LIMITS.shortText),
    path: text(call, "path", LIMITS.path),
    status,
    outcome: outcome as CallOutcome,
    streamed: call.streamed,
    model: optionalText(call, "model", LIMITS.text),
    requestId: optionalText(call, "requestId", LIMITS.text),
    stopReason: optionalText(call, "stopReason", LIMITS.shortText),
    errorType: optionalText(call, "errorType", LIMITS.shortText),
    usage: usage(call.usage),
    startedAtMs: count(call, "startedAtMs"),
    ttfbMs: optionalCount(call, "ttfbMs"),
    durationMs: count(call, "durationMs"),
    rateLimitHeaders: headers(call.rateLimitHeaders),
    traceId: text(call, "traceId", LIMITS.shortText),
    userAgent: optionalText(call, "userAgent", LIMITS.text),
  };
}

/**
 * A {@link CallsPage} from a `calls.list` result, or null when the result isn't
 * one: a wrong type anywhere, more calls than were asked for, calls not newest
 * first, or a `nextBefore` that isn't a position at or before the oldest call.
 */
export function parseCallsList(result: unknown): CallsPage | null {
  try {
    if (!isObject(result) || !Array.isArray(result.calls)) {
      return null;
    }
    if (result.calls.length > LIMITS.pageSize) {
      return null;
    }
    const nextBefore = optionalCount(result, "nextBefore");
    const calls = result.calls.map(entry);
    // Newest first, and the cursor no newer than the oldest call.
    const newestFirst = calls.every(
      (call, i) => i === 0 || call.globalPos < calls[i - 1].globalPos,
    );
    const oldest = calls.at(-1)?.globalPos ?? Number.MAX_SAFE_INTEGER;
    if (
      nextBefore === 0 ||
      !newestFirst ||
      (nextBefore !== null && nextBefore > oldest)
    ) {
      return null;
    }
    return { state: "loaded", calls, nextBefore };
  } catch (error) {
    if (error instanceof Malformed) {
      return null;
    }
    throw error;
  }
}

/** A `before` cursor from the renderer: a positive safe integer, or null. */
export function parseBefore(value: unknown): number | null {
  return isCount(value) && value > 0 ? value : null;
}
