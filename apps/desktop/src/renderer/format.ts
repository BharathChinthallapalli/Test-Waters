/**
 * Formatting for the status screen. Numbers and units are joined with a
 * no-break space so a value never wraps between "2" and "h".
 */

import type { CommandPlatform } from "../shared/daemon-status.ts";

const NBSP = " ";
const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * A duration as people say it, to the two largest units: "45 s", "14 min",
 * "2 h 14 min", "3 d 4 h". A zero second unit is dropped ("2 h").
 */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms));
  if (total < MINUTE) {
    return unit(Math.floor(total / SECOND), "s");
  }
  if (total < HOUR) {
    return unit(Math.floor(total / MINUTE), "min");
  }
  if (total < DAY) {
    return pair(
      Math.floor(total / HOUR),
      "h",
      Math.floor((total % HOUR) / MINUTE),
      "min",
    );
  }
  return pair(
    Math.floor(total / DAY),
    "d",
    Math.floor((total % DAY) / HOUR),
    "h",
  );
}

function unit(value: number | string, name: string): string {
  return `${value}${NBSP}${name}`;
}

function pair(
  first: number,
  firstName: string,
  second: number,
  secondName: string,
): string {
  return second === 0
    ? unit(first, firstName)
    : `${unit(first, firstName)} ${unit(second, secondName)}`;
}

/** An integer with the locale's digit grouping, e.g. "12,408". */
export function formatCount(value: number, locale?: string): string {
  return new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(
    value,
  );
}

/** A wall-clock time with seconds, in the locale's format, e.g. "14:02:31". */
export function formatClock(ms: number, locale?: string): string {
  return new Date(ms).toLocaleTimeString(locale, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/**
 * The command that starts the daemon, as typed into the platform's usual shell.
 * A custom data directory is passed on. It is left bare only when every
 * character is safe unquoted in that shell; otherwise it is quoted so that
 * nothing in it is expanded:
 * - POSIX shells (Linux, macOS): `'…'`, with each `'` written `'\''`;
 * - Windows: `"…"`, which cmd.exe and PowerShell both take literally unless it
 *   holds `%` (cmd.exe), `$` or a backtick (PowerShell), or a double quote
 *   (PowerShell also reads U+201C, U+201D and U+201E as one). A trailing `\`
 *   is also left out of double quotes, where it would escape the closing quote
 *   for the program's argument parser. Those rare paths get PowerShell's
 *   `'…'`, with each single quote doubled; PowerShell reads U+2018 to U+201B
 *   as single quotes too, so those are doubled as well (about_Quoting_Rules;
 *   `CharExtensions.IsSingleQuote` and `IsDoubleQuote` in PowerShell).
 */
export function startCommand(
  dataDir: string | null,
  platform: CommandPlatform,
): string {
  if (dataDir === null) {
    return "cs-daemon";
  }
  const quoted =
    platform === "windows" ? quoteWindows(dataDir) : quotePosix(dataDir);
  return `cs-daemon --data-dir ${quoted}`;
}

/** Plain path characters. No `\`, which is an escape in POSIX shells. */
const POSIX_SAFE = /^[A-Za-z0-9_@%+=:,./-]+$/;

/**
 * Plain path characters. No `,` (the array operator), `@` (splatting) or `%`
 * (an alias), which PowerShell can read as syntax.
 */
const WINDOWS_SAFE = /^[A-Za-z0-9_.:\\/-]+$/;

/** Not literal inside double quotes in cmd.exe or PowerShell. */
const DOUBLE_QUOTE_UNSAFE = /[%$`"“”„]|\\$/;

/** U+0027 and U+2018, U+2019, U+201A, U+201B. */
const POWERSHELL_SINGLE_QUOTES = /['‘’‚‛]/g;

function quotePosix(value: string): string {
  return POSIX_SAFE.test(value) ? value : `'${value.replaceAll("'", "'\\''")}'`;
}

function quoteWindows(value: string): string {
  if (WINDOWS_SAFE.test(value)) {
    return value;
  }
  if (!DOUBLE_QUOTE_UNSAFE.test(value)) {
    return `"${value}"`; // cmd.exe and PowerShell
  }
  return `'${value.replace(POWERSHELL_SINGLE_QUOTES, "$&$&")}'`; // PowerShell
}

/** "812 ms", "3.2 s", "42 s", "2 min 5 s", then {@link formatDuration}'s units. */
export function formatCallDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms));
  if (total < SECOND) {
    return unit(total, "ms");
  }
  if (total < 10 * SECOND) {
    return unit((Math.floor(total / 100) / 10).toFixed(1), "s");
  }
  if (total < MINUTE) {
    return unit(Math.floor(total / SECOND), "s");
  }
  if (total < HOUR) {
    return pair(
      Math.floor(total / MINUTE),
      "min",
      Math.floor((total % MINUTE) / SECOND),
      "s",
    );
  }
  return formatDuration(total);
}

/** When a call started: short text for the list, and the full time on hover. */
export interface CallTime {
  text: string;
  title: string;
}

/**
 * Relative for a call started today ("Just now", "12 min ago", "3 h ago"),
 * else the date ("Sep 26", or "Sep 26, 2025" in another year). `title` is the
 * full date and time. A start in the future (clocks disagree) is "Just now".
 */
export function formatCallTime(
  startedAtMs: number,
  nowMs: number,
  locale?: string,
): CallTime {
  const started = new Date(startedAtMs);
  const now = new Date(nowMs);
  const title = started.toLocaleString(locale, {
    dateStyle: "medium",
    timeStyle: "medium",
  });
  const ago = nowMs - startedAtMs;
  if (ago < MINUTE) {
    return { text: "Just now", title };
  }
  if (started.toDateString() === now.toDateString()) {
    const text =
      ago < HOUR
        ? `${unit(Math.floor(ago / MINUTE), "min")} ago`
        : `${unit(Math.floor(ago / HOUR), "h")} ago`;
    return { text, title };
  }
  const text = started.toLocaleDateString(locale, {
    month: "short",
    day: "numeric",
    ...(started.getFullYear() === now.getFullYear()
      ? {}
      : { year: "numeric" as const }),
  });
  return { text, title };
}

/** Token counts as `calls.list` gives them; the cache counts may be absent. */
export interface TokenUsage {
  inputTokens: number;
  outputTokens: number;
  cacheCreationInputTokens: number | null;
  cacheReadInputTokens: number | null;
}

/**
 * Everything the model read: uncached input plus the tokens written to and read
 * from the prompt cache, which the API reports separately.
 */
export function totalInputTokens(usage: TokenUsage): number {
  return (
    usage.inputTokens +
    (usage.cacheCreationInputTokens ?? 0) +
    (usage.cacheReadInputTokens ?? 0)
  );
}

/**
 * "12,408 input tokens: 408 uncached, 12,000 read from cache, 0 written to
 * cache". A cache count the provider didn't report is left out, not shown as 0.
 */
export function inputTokensBreakdown(
  usage: TokenUsage,
  locale?: string,
): string {
  const parts = [`${formatCount(usage.inputTokens, locale)} uncached`];
  if (usage.cacheReadInputTokens !== null) {
    parts.push(
      `${formatCount(usage.cacheReadInputTokens, locale)} read from cache`,
    );
  }
  if (usage.cacheCreationInputTokens !== null) {
    parts.push(
      `${formatCount(usage.cacheCreationInputTokens, locale)} written to cache`,
    );
  }
  return `${formatCount(totalInputTokens(usage), locale)} input tokens: ${parts.join(", ")}`;
}

/** A command to type, with the shell it is for when there is a choice. */
export interface ShellCommand {
  shell: string | null;
  text: string;
}

/** The proxy's address as the daemon reports it: `127.0.0.1:<port>`. */
const PROXY_ADDRESS = /^127\.0\.0\.1:([1-9][0-9]{0,4})$/;

/** True for `127.0.0.1:<1..65535>`, written without leading zeros. */
export function isProxyAddress(address: string): boolean {
  const port = Number(PROXY_ADDRESS.exec(address)?.[1] ?? 0);
  return port >= 1 && port <= 65_535;
}

/**
 * The line that points Claude Code at the proxy, as typed into the platform's
 * shells, or null for anything but a `127.0.0.1:<port>` address: the line is
 * run in a shell, so nothing else is put into it. The URL needs no quoting in
 * sh, and none in cmd.exe, where quotes would become part of the value;
 * PowerShell assigns a string. Syntax: https://code.claude.com/docs/en/llm-gateway-connect
 * ("Bash or Zsh", "PowerShell") and cmd.exe's `set` (Microsoft Learn, "set
 * (environment variable)").
 */
export function baseUrlCommands(
  address: string,
  platform: CommandPlatform,
): ShellCommand[] | null {
  if (!isProxyAddress(address)) {
    return null;
  }
  const url = `http://${address}`;
  if (platform === "posix") {
    return [{ shell: null, text: `export ANTHROPIC_BASE_URL=${url}` }];
  }
  return [
    { shell: "PowerShell", text: `$env:ANTHROPIC_BASE_URL = "${url}"` },
    { shell: "Command Prompt", text: `set ANTHROPIC_BASE_URL=${url}` },
  ];
}
