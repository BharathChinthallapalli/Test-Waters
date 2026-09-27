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

function unit(value: number, name: string): string {
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
