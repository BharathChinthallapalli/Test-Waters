import type { DaemonStatus } from "../shared/daemon-status.ts";
import type { CallSummary } from "../shared/recent-calls.ts";
import { type CallsList, MAX_HELD_CALLS } from "./calls-state.ts";
import {
  baseUrlCommands,
  formatCallDuration,
  formatCallTime,
  formatCount,
  inputTokensBreakdown,
  type ShellCommand,
  totalInputTokens,
} from "./format.ts";

/**
 * What the "Connect Claude Code" card and the "Recent calls" list show: all of
 * their copy, in one place, pure like `describe.ts`.
 */

/** The path Claude Code sends every model call to; any other is shown. */
export const MESSAGES_PATH = "/v1/messages";

export const EMPTY_TEXT =
  "No calls yet. Point Claude Code at the address above and they appear here.";

/** An empty page that still has a `nextBefore`: records it couldn't read. */
export const UNREADABLE_RECENT_TEXT =
  "The newest recorded calls couldn't be read. Older ones may still be listed.";

/**
 * Setting only `ANTHROPIC_BASE_URL` leaves a saved claude.ai login active
 * (https://code.claude.com/docs/en/llm-gateway, "Subscriptions and gateways");
 * a key or token the user already sets is forwarded unchanged.
 */
export const LOGIN_NOTE =
  "Your existing login keeps working: Claude Code signs in as it does now, only through Callsheet.";

export type ConnectView =
  | { kind: "connect"; text: string; commands: ShellCommand[]; after: string }
  | { kind: "unavailable"; title: string; text: string };

/** Null when the daemon isn't running: the status screen covers that. */
export function describeConnect(status: DaemonStatus): ConnectView | null {
  if (status.state !== "running") {
    return null;
  }
  const proxy = status.health?.proxy ?? null;
  if (proxy === null) {
    return {
      kind: "unavailable",
      title: "Call recording isn't available",
      text: "This daemon doesn't run the proxy that records Claude Code's calls. Start a newer cs-daemon to record them.",
    };
  }
  const commands = baseUrlCommands(proxy.address, status.platform);
  if (commands === null) {
    return {
      kind: "unavailable",
      title: "Call recording isn't available",
      text: "The daemon reported a proxy address other than 127.0.0.1, so the app won't show a command for it.",
    };
  }
  return {
    kind: "connect",
    text:
      commands.length === 1
        ? "Run this in the terminal you start Claude Code from:"
        : "Run one of these in the terminal you start Claude Code from:",
    commands,
    after: LOGIN_NOTE,
  };
}

/** A call's status mark: its own shape and its own text in every tone. */
export type CallTone = "ok" | "off" | "warning" | "danger";

export interface CallOutcomeView {
  label: string;
  tone: CallTone;
}

export function describeOutcome(call: CallSummary): CallOutcomeView {
  switch (call.outcome) {
    case "completed":
      return { label: "Completed", tone: "ok" };
    case "upstreamError":
      // A stream can carry an `error` event after a 200.
      return call.status >= 400
        ? { label: `Error ${call.status}`, tone: "danger" }
        : { label: "Stream error", tone: "danger" };
    case "clientCancelled":
      return { label: "Cancelled", tone: "off" };
    case "upstreamUnreachable":
      return { label: "Unreachable", tone: "danger" };
    case "incomplete":
      return { label: "Incomplete", tone: "warning" };
  }
}

export interface CallDetail {
  key: string;
  label: string;
  value: string;
  mono?: boolean;
}

export interface CallRowView {
  /** The call's global position, as a string: stable across refreshes. */
  key: string;
  time: string;
  timeTitle: string;
  /** Null when the provider didn't name one. */
  model: string | null;
  /** Shown only when it isn't {@link MESSAGES_PATH}. */
  path: string | null;
  streamed: boolean;
  outcome: CallOutcomeView;
  /** "—" when the provider reported no usage; never "0". */
  tokensIn: string;
  tokensInTitle: string | null;
  tokensOut: string;
  duration: string;
  /** The row's accessible name: what a sighted reader gets from the row. */
  label: string;
  details: CallDetail[];
  /** Recorded rate-limit and retry headers, without `request-id`. */
  headers: Array<[string, string]>;
}

const NO_USAGE = "—";

export function describeCall(
  call: CallSummary,
  nowMs: number,
  locale?: string,
): CallRowView {
  const time = formatCallTime(call.startedAtMs, nowMs, locale);
  const outcome = describeOutcome(call);
  const usage = call.usage;
  const tokensIn = usage
    ? formatCount(totalInputTokens(usage), locale)
    : NO_USAGE;
  const tokensOut = usage ? formatCount(usage.outputTokens, locale) : NO_USAGE;
  const duration = formatCallDuration(call.durationMs);
  const path = call.path === MESSAGES_PATH ? null : call.path;

  const label = [
    time.text,
    call.model ?? "Model not reported",
    path,
    call.streamed ? "streamed" : null,
    outcome.label,
    usage
      ? `${tokensIn} tokens in, ${tokensOut} tokens out`
      : "no token counts",
    duration,
  ]
    .filter((part) => part !== null)
    .join(", ");

  const details: CallDetail[] = [
    { key: "started", label: "Started", value: time.title },
    {
      key: "request",
      label: "Request",
      value: `${call.method} ${call.path}`,
      mono: true,
    },
    { key: "status", label: "HTTP status", value: String(call.status) },
  ];
  if (call.errorType !== null) {
    details.push({
      key: "error-type",
      label: "Error type",
      value: call.errorType,
      mono: true,
    });
  }
  if (call.stopReason !== null) {
    details.push({
      key: "stop-reason",
      label: "Stop reason",
      value: call.stopReason,
      mono: true,
    });
  }
  details.push({
    key: "tokens",
    label: "Tokens",
    value: usage
      ? `${inputTokensBreakdown(usage, locale)}. ${tokensOut} output tokens.`
      : "Not reported by the provider.",
  });
  if (call.ttfbMs !== null) {
    details.push({
      key: "ttfb",
      label: "First response",
      value: `after ${formatCallDuration(call.ttfbMs)}`,
    });
  }
  details.push(
    {
      key: "request-id",
      label: "Request ID",
      value: call.requestId ?? "Not reported",
      mono: call.requestId !== null,
    },
    { key: "run", label: "Run ID", value: call.runId, mono: true },
    { key: "trace", label: "Trace ID", value: call.traceId, mono: true },
  );
  if (call.userAgent !== null) {
    details.push({
      key: "client",
      label: "Client",
      value: call.userAgent,
      mono: true,
    });
  }

  return {
    key: String(call.globalPos),
    time: time.text,
    timeTitle: time.title,
    model: call.model,
    path,
    streamed: call.streamed,
    outcome,
    tokensIn,
    tokensInTitle: usage ? inputTokensBreakdown(usage, locale) : null,
    tokensOut,
    duration,
    label,
    details,
    headers: call.rateLimitHeaders.filter(([name]) => name !== "request-id"),
  };
}

export type LoadOlder = "hidden" | "ready" | "loading";

export interface CallsView {
  /** "list" shows rows; the others show `message` in place of them. */
  state: "list" | "empty" | "error";
  message: string | null;
  detail: string | null;
  /** A problem refreshing a list that is still shown. */
  note: string | null;
  /** Calls the proxy forwarded but couldn't record. */
  dropped: string | null;
  rows: CallRowView[];
  loadOlder: LoadOlder;
  olderError: string | null;
  /** Said once {@link MAX_HELD_CALLS} are shown and older ones remain. */
  limitNote: string | null;
}

export interface CallsUi {
  loadingOlder: boolean;
  olderError: string | null;
}

/**
 * The Recent calls section, or null when it isn't shown: the daemon isn't
 * running, or runs no proxy and has no recorded calls to show.
 */
export function describeCalls(
  status: DaemonStatus,
  list: CallsList,
  ui: CallsUi,
  nowMs: number,
  locale?: string,
): CallsView | null {
  if (status.state !== "running") {
    return null;
  }
  const proxy = status.health?.proxy ?? null;
  if (proxy === null && list.calls.length === 0) {
    return null;
  }
  const dropped =
    proxy && proxy.recordsDropped > 0
      ? `${formatCount(proxy.recordsDropped, locale)} ${proxy.recordsDropped === 1 ? "call" : "calls"} since the daemon started went through but weren't recorded.`
      : null;
  const base = {
    note: null,
    dropped,
    rows: [],
    loadOlder: "hidden" as const,
    olderError: null,
    limitNote: null,
  };
  const problem = list.problem;

  if (list.calls.length === 0) {
    if (problem?.state === "failed") {
      return {
        ...base,
        state: "error",
        message: `${problem.message} The app tries again every 3 seconds.`,
        detail: problem.detail ?? null,
      };
    }
    if (problem?.state === "unsupported") {
      return {
        ...base,
        state: "error",
        message:
          "This daemon can't list recorded calls. Start a newer cs-daemon to see them here.",
        detail: null,
      };
    }
    if (list.nextBefore !== null) {
      // The newest records couldn't be read, but older ones may exist: only a
      // page without `nextBefore` ends the list.
      return {
        ...base,
        state: "empty",
        message: UNREADABLE_RECENT_TEXT,
        detail: null,
        loadOlder: ui.loadingOlder ? "loading" : "ready",
        olderError: ui.olderError,
      };
    }
    return { ...base, state: "empty", message: EMPTY_TEXT, detail: null };
  }

  const atLimit =
    list.calls.length >= MAX_HELD_CALLS && list.nextBefore !== null;
  return {
    ...base,
    state: "list",
    message: null,
    detail: null,
    note:
      problem?.state === "failed"
        ? `Couldn't refresh the list: ${problem.message} Showing the calls loaded before.`
        : null,
    rows: list.calls.map((call) => describeCall(call, nowMs, locale)),
    loadOlder:
      list.nextBefore === null || atLimit
        ? "hidden"
        : ui.loadingOlder
          ? "loading"
          : "ready",
    olderError: ui.olderError,
    limitNote: atLimit
      ? `Showing the newest ${formatCount(MAX_HELD_CALLS, locale)} calls.`
      : null,
  };
}
