/**
 * Formatting for the status screen. Numbers and units are joined with a
 * no-break space so a value never wraps between "2" and "h".
 */

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
 * The command that starts the daemon. A custom data directory is passed on,
 * quoted for the shell when it has anything but plain path characters: single
 * quotes for POSIX shells (and PowerShell, which also takes them literally),
 * so nothing in the path is expanded.
 */
export function startCommand(dataDir: string | null): string {
  if (dataDir === null) {
    return "cs-daemon";
  }
  return `cs-daemon --data-dir ${quoteArgument(dataDir)}`;
}

function quoteArgument(value: string): string {
  if (/^[\w@%+=:,./\\-]+$/.test(value)) {
    return value;
  }
  return `'${value.replaceAll("'", "'\\''")}'`;
}
