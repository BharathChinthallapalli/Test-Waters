/**
 * What the desktop app knows about the local daemon. The main process builds it
 * (`src/main/daemon.ts`), the preload passes it through unchanged and the
 * renderer only displays it. It never contains the control token or any other
 * secret: everything here may be shown on screen.
 *
 * Every variant carries `message`, one plain-language sentence saying what
 * happened; `checkedAtMs`, when the check that produced it finished (0 before
 * the first check); and `customDataDir`, true when the data directory came from
 * `CALLSHEET_DATA_DIR`, so starting the daemon needs `--data-dir`.
 */
export type DaemonStatus =
  | ConnectingStatus
  | StartingStatus
  | NotRunningStatus
  | RunningStatus
  | UnauthorizedStatus
  | UnreachableStatus
  | ErrorStatus;

interface StatusBase {
  message: string;
  checkedAtMs: number;
  customDataDir: boolean;
}

/** The app has not finished its first check yet. */
export interface ConnectingStatus extends StatusBase {
  state: "connecting";
}

/** A daemon holds the lock but has not published its address yet. */
export interface StartingStatus extends StatusBase {
  state: "starting";
  dataDir: string;
  pid: number;
}

/** No daemon for this data directory: no `daemon.json`, or a stale one. */
export interface NotRunningStatus extends StatusBase {
  state: "not-running";
  dataDir: string;
  /** A leftover `daemon.json` named a process that is gone. */
  stale: boolean;
}

/** Health fields, present only when the daemon implements `health`. */
export interface DaemonHealth {
  captureContent: boolean;
  /** Global position of the newest event, which is also the number of events. */
  lastGlobalPosition: number;
  erasurePending: boolean;
}

/** The daemon answered `version` (and `health`, when it has it). */
export interface RunningStatus extends StatusBase {
  state: "running";
  dataDir: string;
  address: string;
  pid: number;
  daemonVersion: string;
  schemaVersion: number;
  /** From `health`, or estimated from `daemon.json`'s start time. */
  uptimeMs: number;
  /** Null when the daemon predates the `health` method (JSON-RPC -32601). */
  health: DaemonHealth | null;
}

/** The daemon rejected the token twice, re-reading the token file in between. */
export interface UnauthorizedStatus extends StatusBase {
  state: "unauthorized";
  dataDir: string;
  address: string;
}

/** A live daemon process is registered, but its address refused or timed out. */
export interface UnreachableStatus extends StatusBase {
  state: "unreachable";
  dataDir: string;
  address: string;
  pid: number;
}

/** Anything else: unreadable files, a malformed record, an unexpected reply. */
export interface ErrorStatus extends StatusBase {
  state: "error";
  dataDir: string | null;
  /** What went wrong, so the screen can suggest the matching next step. */
  reason: ErrorReason;
}

export type ErrorReason =
  /** No home directory and no `CALLSHEET_DATA_DIR`. */
  | "no-data-dir"
  /** `daemon.json` or `control-token` exists but can't be read. */
  | "unreadable"
  /** `daemon.json` isn't a record the daemon writes. */
  | "invalid-record"
  /** `daemon.json` names an address other than `127.0.0.1:<port>`. */
  | "not-loopback"
  /** `control-token` is missing or isn't 64 lowercase hex characters. */
  | "bad-token"
  /** The program at the address didn't answer like the daemon. */
  | "protocol"
  /** The daemon answered with a JSON-RPC error, or something else failed. */
  | "unexpected";

export type DaemonState = DaemonStatus["state"];
