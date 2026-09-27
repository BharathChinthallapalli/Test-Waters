import type { DaemonStatus, ErrorStatus } from "../shared/daemon-status.ts";
import { formatCount, formatDuration, startCommand } from "./format.ts";

/**
 * What the status screen shows for a {@link DaemonStatus}: all of its copy, in
 * one place. Pure, so every state's wording is tested without a DOM.
 *
 * Copy rules (`.kiro/steering/ui.md`): say what happened, then what to do next;
 * name things the way the user will see them in a terminal.
 */

/** The status mark: each tone has its own shape as well as its own colour. */
export type Tone = "ok" | "off" | "pending" | "warning" | "danger";

export interface DetailRow {
  /** Stable identity, so an update changes text in place. */
  key: string;
  label: string;
  value: string;
  /** Technical values (addresses, paths) are set in monospace. */
  mono?: boolean;
  /** Shown dimmed: a value the daemon didn't report. */
  muted?: boolean;
  /** One line under the value explaining what it means. */
  note?: string;
}

export interface NextStep {
  text: string;
  /** A command to run, shown copyable. */
  command?: string;
  /** A sentence after the command. */
  after?: string;
}

export interface Notice {
  tone: "info" | "warning";
  title: string;
  text: string;
}

export interface StatusView {
  tone: Tone;
  headline: string;
  message: string;
  notice: Notice | null;
  nextStep: NextStep | null;
  rows: DetailRow[];
}

export const CAPTURE_ON_NOTE = "Message content is stored on this machine.";
/** Matches PRIVACY.md, "Content capture and your keychain". */
export const CAPTURE_OFF_NOTE =
  "New message content isn't stored, only call metadata. Content stored while capture was on stays until you erase it.";

export function describeStatus(status: DaemonStatus): StatusView {
  const view = describeState(status);
  if (status.detail !== undefined) {
    view.rows.push({
      key: "detail",
      label: "Technical detail",
      value: status.detail,
      mono: true,
    });
  }
  return view;
}

function describeState(status: DaemonStatus): StatusView {
  switch (status.state) {
    case "connecting":
      return {
        tone: "pending",
        headline: "Checking for the daemon",
        message: status.message,
        notice: null,
        nextStep: null,
        rows: [],
      };

    case "starting":
      return {
        tone: "pending",
        headline: "Daemon starting",
        message: status.message,
        notice: null,
        nextStep: {
          text: `This usually takes a moment, and the screen updates by itself. If it lasts more than a minute, stop the daemon (process ${status.pid}) and start it again:`,
          command: command(status),
        },
        rows: [
          { key: "pid", label: "Process", value: String(status.pid) },
          dataDirRow(status.dataDir),
        ],
      };

    case "not-running":
      return {
        tone: "off",
        headline: "Daemon not running",
        message: status.message,
        notice: null,
        nextStep: {
          text: "Start it in a terminal:",
          command: command(status),
          after: "This screen updates by itself once it's running.",
        },
        rows: [dataDirRow(status.dataDir)],
      };

    case "running":
      return {
        tone: "ok",
        headline: "Daemon running",
        message: status.message,
        notice: runningNotice(status),
        nextStep: null,
        rows: [
          {
            key: "address",
            label: "Address",
            value: status.address,
            mono: true,
          },
          { key: "version", label: "Version", value: status.daemonVersion },
          {
            key: "schema",
            label: "Schema version",
            value: String(status.schemaVersion),
          },
          {
            key: "uptime",
            label: "Uptime",
            value: formatDuration(status.uptimeMs),
          },
          status.health
            ? {
                key: "capture",
                label: "Content capture",
                value: status.health.captureContent ? "On" : "Off",
                note: status.health.captureContent
                  ? CAPTURE_ON_NOTE
                  : CAPTURE_OFF_NOTE,
              }
            : notReported("capture", "Content capture"),
          status.health
            ? {
                key: "events",
                label: "Events recorded",
                value: formatCount(status.health.lastGlobalPosition),
              }
            : notReported("events", "Events recorded"),
        ],
      };

    case "unhealthy":
      return {
        tone: "warning",
        headline: "Daemon unhealthy",
        message: status.message,
        notice: null,
        nextStep: {
          text: `It may recover by itself. If this lasts more than a minute, stop the daemon (process ${status.pid}) and start it again:`,
          command: command(status),
        },
        rows: [
          {
            key: "address",
            label: "Address",
            value: status.address,
            mono: true,
          },
          { key: "version", label: "Version", value: status.daemonVersion },
          { key: "pid", label: "Process", value: String(status.pid) },
        ],
      };

    case "unauthorized":
      return {
        tone: "warning",
        headline: "Daemon refused access",
        message: status.message,
        notice: null,
        nextStep: {
          text: "The app re-reads the token file on every check, so a rotated token clears this by itself. If it stays, restart the daemon.",
        },
        rows: [
          {
            key: "address",
            label: "Address",
            value: status.address,
            mono: true,
          },
          {
            key: "token",
            label: "Token file",
            value: joinPath(status.dataDir, "control-token"),
            mono: true,
          },
        ],
      };

    case "unreachable":
      return {
        tone: "danger",
        headline: "Daemon not responding",
        message: status.message,
        notice: null,
        nextStep: {
          text: `It may be busy or stuck. If this lasts more than a minute, stop the daemon (process ${status.pid}) and start it again:`,
          command: command(status),
        },
        rows: [
          {
            key: "address",
            label: "Address",
            value: status.address,
            mono: true,
          },
          { key: "pid", label: "Process", value: String(status.pid) },
          dataDirRow(status.dataDir),
        ],
      };

    case "error":
      return {
        tone: "danger",
        headline:
          status.reason === "app"
            ? "Callsheet can't show the status"
            : "Can't check the daemon",
        message: status.message,
        notice: null,
        nextStep: errorNextStep(status),
        rows: status.dataDir ? [dataDirRow(status.dataDir)] : [],
      };
  }
}

const RESTARTED = "This screen updates by itself once it's running again.";

function command(status: DaemonStatus & { dataDir: string | null }): string {
  return startCommand(
    status.customDataDir ? status.dataDir : null,
    status.platform,
  );
}

function errorNextStep(status: ErrorStatus): NextStep {
  const start = command(status);
  switch (status.reason) {
    case "no-data-dir":
      return {
        text: "Set CALLSHEET_DATA_DIR to the daemon's data directory, then reopen the app.",
      };
    case "unreadable":
      return {
        text: "Make sure the data directory and its files belong to your user. The daemon creates them readable only by their owner.",
      };
    case "invalid-record":
      return {
        text: "Restart the daemon so it writes the file again:",
        command: start,
        after: RESTARTED,
      };
    case "not-loopback":
      return {
        text: "Callsheet's daemon only listens on 127.0.0.1, so another program may have written this file. Restart the daemon to replace it:",
        command: start,
        after: RESTARTED,
      };
    case "bad-token":
      return {
        text: "Restart the daemon: it creates a missing token file. If the file is damaged, delete it first.",
        command: start,
        after: RESTARTED,
      };
    case "protocol":
      return {
        text: "Another program may have taken the daemon's port. Restart the daemon so it publishes a new address:",
        command: start,
        after: RESTARTED,
      };
    case "unexpected":
      return {
        text: "The app keeps checking. If this stays, restart the daemon:",
        command: start,
      };
    case "app":
      return {
        text: "Restart Callsheet. Restarting the app doesn't stop the daemon.",
      };
  }
}

function runningNotice(
  status: Extract<DaemonStatus, { state: "running" }>,
): Notice | null {
  if (!status.health) {
    return {
      tone: "info",
      title: "Some details need a newer daemon",
      text: "This daemon doesn't report its health, so content capture and the event count are unknown. Uptime is worked out from when it started.",
    };
  }
  if (status.health.erasurePending) {
    return {
      tone: "warning",
      title: "Erasure pending",
      text: "Some erased message content may still be in the database's write-ahead log or a migration backup. The daemon retries every 30 seconds and at its next start; this clears once every copy is gone.",
    };
  }
  return null;
}

function dataDirRow(dataDir: string): DetailRow {
  return {
    key: "data-dir",
    label: "Data directory",
    value: dataDir,
    mono: true,
  };
}

function notReported(key: string, label: string): DetailRow {
  return { key, label, value: "Not reported", muted: true };
}

/** Joins with the separator `dir` already uses, so Windows paths stay Windows. */
function joinPath(dir: string, name: string): string {
  const separator = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  return dir.endsWith(separator) ? dir + name : dir + separator + name;
}
