import { readFile, stat } from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import type {
  CallsListParams,
  HealthResult,
  ProxyHealth,
  VersionResult,
} from "@callsheet/api-types";
import {
  commandPlatform,
  type DaemonHealth,
  type DaemonStatus,
} from "../shared/daemon-status.ts";
import type { RecentCalls } from "../shared/recent-calls.ts";
import { LIMITS, parseCallsList } from "./calls.ts";

/**
 * The desktop app's client for the local daemon (feature 02, task 12).
 *
 * Only the main process talks to the daemon (ADR 0003). It finds the daemon the
 * same way `cs-daemon` publishes itself (`crates/cs-daemon/src/{config,instance,
 * token}.rs`), reads the control token, and calls the JSON-RPC API with Node's
 * HTTP client, never through the renderer's session (which only allows `app://`).
 * The token stays in this module: it is never logged, never put in a status and
 * only ever sent to `127.0.0.1`.
 *
 * **Data directory.** `CALLSHEET_DATA_DIR` wins when set (tests and development;
 * it must match the daemon's `--data-dir`; give an absolute path, since a
 * relative one is resolved against the app's working directory). Otherwise it
 * is the daemon's default, which comes from etcetera 0.11.0's native app
 * strategy with the app name `Callsheet` (`src/app_strategy.rs`,
 * `src/{app,base}_strategy/{xdg,apple,windows}.rs`):
 * - Linux and other Unix: `$XDG_DATA_HOME/callsheet` when `XDG_DATA_HOME` is an
 *   absolute path, else `~/.local/share/callsheet`;
 * - macOS: `~/Library/Application Support/Callsheet`;
 * - Windows: `%APPDATA%\Callsheet\data`, or `~\AppData\Roaming\Callsheet\data` when
 *   `APPDATA` is empty. (etcetera asks the shell for the Roaming folder before
 *   falling back to the home directory; Node can't, and the two agree on a
 *   standard install.)
 * A daemon started with another `--data-dir` is only found through the variable.
 *
 * **Discovery.** `daemon.json` is trusted only when its address is `127.0.0.1`
 * with a non-zero port and its pid is still the daemon that wrote it:
 * - the pid is alive (`process.kill(pid, 0)`, which works on Windows too);
 * - the record was written after this boot (`os.uptime()`), so a pid reused
 *   after a reboot doesn't count (except after a Windows Fast Startup shutdown,
 *   which hibernates the kernel and keeps its uptime running);
 * - on Linux, `/proc/<pid>/stat` shows the process hasn't exited (a zombie, `Z`,
 *   still passes the first check) and didn't start after the record was written
 *   (the pid was reused);
 * - on Unix, the pid matches the one the daemon wrote into `daemon.lock`, as
 *   `cs_daemon::instance::read_discovery` requires. Windows locks that file
 *   against reading while the daemon runs, so the check is skipped there.
 * A live pid in `daemon.lock` that isn't a stale record's pid is a daemon that
 * is starting and hasn't published `daemon.json` yet.
 *
 * **Residual gap.** `read_discovery` also probes that `daemon.lock` is locked;
 * Node has no file-lock call, so this client can't. On Linux the start-time check
 * above catches a reused pid. On macOS, a crashed daemon's pid reused by another
 * process of the same user in the same boot, *and* another program on the old
 * port, would receive the token; on Windows pid reuse and the port suffice.
 * Closing this needs the lock probe (a native module) or a daemon-side proof of
 * identity: a follow-up recorded in the pull request for task 12 (#59).
 *
 * **Clock steps.** The times compared are wall-clock times recorded earlier, so
 * a wall clock stepped forward by more than {@link START_SLACK_MS} after the
 * daemon started makes it look stale on Linux (more than
 * {@link BOOT_SLACK_MS} elsewhere): the screen says it isn't running and nothing
 * is sent to it, which is the safe way to be wrong. A clock-independent identity
 * in `daemon.json` (the boot id and the process's start ticks) would remove
 * this; it is a follow-up in the same pull request.
 *
 * **Recent calls** (feature 03). A running daemon with `health` is also asked
 * for the newest page of `calls.list` on every check, after `version` and
 * `health` answered, so a failure there never changes the daemon's status; it
 * only makes the calls "failed". {@link DaemonMonitor.listCalls} fetches an
 * older page for "Load older", from the daemon the last check found running.
 */

export const DISCOVERY_FILE_NAME = "daemon.json";
export const LOCK_FILE_NAME = "daemon.lock";
export const TOKEN_FILE_NAME = "control-token";
export const DATA_DIR_ENV = "CALLSHEET_DATA_DIR";

/** The only host the client ever connects to (R2.7). */
const LOOPBACK = "127.0.0.1";
const RPC_PATH = "/rpc";

/** Each request, including connecting, must finish within this long. */
export const REQUEST_TIMEOUT_MS = 2000;

/**
 * A daemon whose `daemon.lock` was written this recently, but which has no
 * `daemon.json` yet, is reported as starting rather than not running.
 */
export const STARTING_GRACE_MS = 10_000;

/**
 * Replies are a few hundred bytes, and a `calls.list` page at most about 48 KiB
 * of entries (#63); anything bigger is not the daemon.
 */
const MAX_RESPONSE_BYTES = 64 * 1024;

/**
 * Slack when comparing a time the daemon recorded (`startedAtMs`, the lock's
 * modification time) with the boot time from `os.uptime()`: whole seconds on
 * macOS and Windows, and small wall-clock corrections since.
 */
export const BOOT_SLACK_MS = 30_000;

/**
 * Slack when comparing a recorded time with the process's start time from
 * `/proc` (`btime` is whole seconds, start ticks 10 ms). The daemon records its
 * start and writes its lock right after the process starts, so a process that
 * started later than this reused the pid.
 */
export const START_SLACK_MS = 5000;

/** JSON-RPC 2.0 "Method not found": a daemon older than the method. */
const METHOD_NOT_FOUND = -32601;

const TOKEN_PATTERN = /^[0-9a-f]{64}$/;
const ADDRESS_PATTERN = /^127\.0\.0\.1:([0-9]{1,5})$/;

type Env = Readonly<Record<string, string | undefined>>;

/** `Omit` applied to each member of a union separately. */
type DistributiveOmit<T, K extends PropertyKey> = T extends unknown
  ? Omit<T, K>
  : never;

/**
 * The data directory the daemon uses by default for this user, or the
 * `CALLSHEET_DATA_DIR` override. Null when there is no home directory to put it
 * in (the daemon refuses to start then too).
 */
export function resolveDataDir(
  env: Env,
  platform: NodeJS.Platform,
  homeDir: string,
): string | null {
  const override = env[DATA_DIR_ENV];
  if (override) {
    return platform === "win32"
      ? path.win32.resolve(override)
      : path.posix.resolve(override);
  }
  if (platform === "win32") {
    const appData = env.APPDATA;
    if (appData) {
      return path.win32.join(appData, "Callsheet", "data");
    }
    return homeDir
      ? path.win32.join(homeDir, "AppData", "Roaming", "Callsheet", "data")
      : null;
  }
  if (platform === "darwin") {
    return homeDir
      ? path.posix.join(homeDir, "Library", "Application Support", "Callsheet")
      : null;
  }
  const xdgDataHome = env.XDG_DATA_HOME;
  if (xdgDataHome && path.posix.isAbsolute(xdgDataHome)) {
    return path.posix.join(xdgDataHome, "callsheet");
  }
  return homeDir
    ? path.posix.join(homeDir, ".local", "share", "callsheet")
    : null;
}

/** The fields of `daemon.json` (`cs_daemon::instance::Discovery`). */
export interface Discovery {
  pid: number;
  startedAtMs: number;
  address: string;
  port: number;
  schemaVersion: number;
}

/**
 * Validates a parsed `daemon.json`: a record the daemon could have written, with
 * the address `127.0.0.1:<non-zero port>`. Otherwise says what is wrong.
 */
export function parseDiscovery(
  value: unknown,
): Discovery | "invalid-record" | "not-loopback" {
  if (typeof value !== "object" || value === null) {
    return "invalid-record";
  }
  const { pid, startedAtMs, address, schemaVersion } = value as Record<
    string,
    unknown
  >;
  if (
    !isCount(pid) ||
    pid === 0 ||
    !isCount(startedAtMs) ||
    !isCount(schemaVersion) ||
    typeof address !== "string"
  ) {
    return "invalid-record";
  }
  const match = ADDRESS_PATTERN.exec(address);
  if (!match) {
    return "not-loopback";
  }
  const port = Number(match[1]);
  if (port < 1 || port > 65_535) {
    return "invalid-record";
  }
  return { pid, startedAtMs, address, port, schemaVersion };
}

function isCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

/**
 * True when a process with this pid exists and belongs to this user. EPERM means
 * it exists but belongs to someone else, so it can't be this user's daemon. A
 * zombie still counts as existing; {@link readLinuxProcessInfo} tells them apart.
 */
export function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** What Linux reports about a process in `/proc/<pid>/stat`. */
export interface ProcessInfo {
  /** Field 3: `R`, `S`, `D`, `T`, `Z` (zombie), `X` (dead), … */
  state: string;
  /** Field 22 (`start_time`, clock ticks after boot) as wall-clock ms. */
  startedAtMs: number;
}

/**
 * `/proc` counts start times in clock ticks of `USER_HZ`, which the kernel ABI
 * fixes at 100 (`include/uapi/asm-generic/param.h`). Node has no way to call
 * `sysconf(_SC_CLK_TCK)` to confirm it.
 */
const USER_HZ = 100;

/** States in which a process has exited: zombie, and dead (`x` before 3.14). */
const EXITED_STATES = new Set(["Z", "X", "x"]);

/**
 * Parses `/proc/<pid>/stat`. Field 2, the command name in parentheses, may hold
 * spaces and parentheses itself, so fields are counted from the last `)`.
 */
export function parseProcessStat(
  stat: string,
  bootTimeMs: number,
): ProcessInfo | null {
  const end = stat.lastIndexOf(")");
  if (end < 0) {
    return null;
  }
  const fields = stat
    .slice(end + 1)
    .trim()
    .split(/\s+/);
  const state = fields[0]; // field 3
  const startTicks = fields[19]; // field 22
  if (!state || !startTicks || !/^[0-9]+$/.test(startTicks)) {
    return null;
  }
  return {
    state,
    startedAtMs: bootTimeMs + (Number(startTicks) * 1000) / USER_HZ,
  };
}

/** The `btime` line of `/proc/stat`: when the system booted, as wall-clock ms. */
export function parseBootTime(procStat: string): number | null {
  const match = /^btime ([0-9]+)$/m.exec(procStat);
  return match ? Number(match[1]) * 1000 : null;
}

/**
 * Reads {@link ProcessInfo} from `/proc`. "gone" when `/proc` works but has no
 * such process (it exited after `process.kill(pid, 0)` succeeded); null when
 * `/proc` can't say.
 */
export async function readLinuxProcessInfo(
  pid: number,
): Promise<ProcessInfo | "gone" | null> {
  let bootTimeMs: number | null;
  try {
    bootTimeMs = parseBootTime(await readFile("/proc/stat", "utf8"));
  } catch {
    return null;
  }
  if (bootTimeMs === null) {
    return null;
  }
  try {
    return parseProcessStat(
      await readFile(`/proc/${pid}/stat`, "utf8"),
      bootTimeMs,
    );
  } catch (error) {
    const code = errorCode(error);
    return code === "ENOENT" || code === "ESRCH" ? "gone" : null;
  }
}

/**
 * When the system booted, as wall-clock ms. `os.uptime()` includes time asleep
 * on every platform (libuv: `/proc/uptime` on Linux, `KERN_BOOTTIME` on macOS,
 * `GetTickCount64` on Windows).
 */
export function systemBootTimeMs(): number {
  return Date.now() - os.uptime() * 1000;
}

/** Why a call to the daemon failed, without any detail from the request. */
export class DaemonCallError extends Error {
  readonly kind:
    | "unauthorized"
    | "refused"
    | "reset"
    | "timeout"
    | "failed"
    | "protocol";
  /** Secondary technical detail for the status: an error code, what was wrong. */
  readonly detail: string | undefined;

  constructor(kind: DaemonCallError["kind"], message: string, detail?: string) {
    super(message);
    this.name = "DaemonCallError";
    this.kind = kind;
    this.detail = detail;
  }
}

/** A JSON-RPC error object returned by the daemon. */
export class RpcError extends Error {
  readonly code: number;

  constructor(code: number, message: string) {
    super(message);
    this.name = "RpcError";
    this.code = code;
  }

  /** For a status's `detail`: "Error -32000: store unavailable". */
  get detail(): string {
    return this.message
      ? `Error ${this.code}: ${this.message}`
      : `Error ${this.code}`;
  }
}

interface HttpReply {
  status: number;
  body: string;
}

/**
 * Posts one JSON-RPC request to `127.0.0.1:<port>/rpc` with the bearer token.
 * A fresh connection per request (`agent: false`): no pooled socket the daemon
 * may have closed, and no global agent that could route through a proxy.
 */
function postRpc(
  port: number,
  token: string,
  body: string,
  timeoutMs: number,
): Promise<HttpReply> {
  return new Promise((resolve, reject) => {
    const fail = (error: unknown): void => reject(classifyNetworkError(error));
    const request = http.request(
      {
        host: LOOPBACK,
        port,
        path: RPC_PATH,
        method: "POST",
        agent: false,
        signal: AbortSignal.timeout(timeoutMs),
        headers: {
          host: `${LOOPBACK}:${port}`,
          authorization: `Bearer ${token}`,
          "content-type": "application/json",
          "content-length": Buffer.byteLength(body),
          accept: "application/json",
        },
      },
      (response) => {
        const chunks: Buffer[] = [];
        let size = 0;
        response.on("data", (chunk: Buffer) => {
          size += chunk.length;
          if (size > MAX_RESPONSE_BYTES) {
            request.destroy(
              new DaemonCallError(
                "protocol",
                "The reply was too large.",
                "Reply over 64 KiB",
              ),
            );
            return;
          }
          chunks.push(chunk);
        });
        response.on("end", () =>
          resolve({
            status: response.statusCode ?? 0,
            body: Buffer.concat(chunks).toString("utf8"),
          }),
        );
        response.on("error", fail);
      },
    );
    request.on("error", fail);
    request.end(body);
  });
}

function classifyNetworkError(error: unknown): DaemonCallError {
  if (error instanceof DaemonCallError) {
    return error;
  }
  const { name, code } = (error ?? {}) as { name?: unknown; code?: unknown };
  if (
    name === "AbortError" ||
    name === "TimeoutError" ||
    code === "ETIMEDOUT"
  ) {
    return new DaemonCallError("timeout", "The daemon didn't answer in time.");
  }
  const detail = typeof code === "string" ? code : undefined;
  switch (code) {
    case "ECONNREFUSED":
      return new DaemonCallError(
        "refused",
        "The connection was refused.",
        detail,
      );
    case "ECONNRESET": // also Node's "socket hang up"
    case "EPIPE":
      return new DaemonCallError(
        "reset",
        "The connection was closed without an answer.",
        detail,
      );
    default:
      return new DaemonCallError("failed", "The connection failed.", detail);
  }
}

/** Checks the daemon once; see the module comment. */
export interface MonitorOptions {
  dataDir: string | null;
  /** `dataDir` came from `CALLSHEET_DATA_DIR` rather than the default. */
  customDataDir?: boolean;
  platform?: NodeJS.Platform;
  isProcessAlive?: (pid: number) => boolean;
  /** Defaults to {@link readLinuxProcessInfo} on Linux, and "unknown" elsewhere. */
  processInfo?: (pid: number) => Promise<ProcessInfo | "gone" | null>;
  /** Defaults to {@link systemBootTimeMs}. */
  bootTimeMs?: () => number;
  now?: () => number;
  timeoutMs?: number;
}

/** `daemon.lock`: the pid the daemon wrote into it, and when it wrote it. */
interface LockFile {
  pid: number;
  writtenAtMs: number;
}

/**
 * Whether a pid is still the process that recorded something: "verified" where
 * the OS reports the process (Linux), "unverified" where it can't.
 */
type Liveness = "dead" | "verified" | "unverified";

/**
 * Checks the daemon and reports a {@link DaemonStatus}. Keeps the control token
 * in memory between checks and re-reads it once after a 401 (R2.9).
 */
export class DaemonMonitor {
  readonly #dataDir: string | null;
  readonly #customDataDir: boolean;
  readonly #platform: NodeJS.Platform;
  readonly #isProcessAlive: (pid: number) => boolean;
  readonly #processInfo: (pid: number) => Promise<ProcessInfo | "gone" | null>;
  readonly #bootTimeMs: () => number;
  readonly #now: () => number;
  readonly #timeoutMs: number;
  #token: string | null = null;
  /** `version` doesn't change while a daemon runs, so it is asked once per run. */
  #version: { run: string; result: VersionResult } | null = null;
  /** Where the last check found a running daemon, for {@link listCalls}. */
  #running: { dataDir: string; discovery: Discovery } | null = null;
  #nextId = 1;

  constructor(options: MonitorOptions) {
    this.#dataDir = options.dataDir;
    this.#customDataDir = options.customDataDir ?? false;
    this.#platform = options.platform ?? process.platform;
    this.#isProcessAlive = options.isProcessAlive ?? isProcessAlive;
    this.#processInfo =
      options.processInfo ??
      (this.#platform === "linux"
        ? readLinuxProcessInfo
        : () => Promise.resolve(null));
    this.#bootTimeMs = options.bootTimeMs ?? systemBootTimeMs;
    this.#now = options.now ?? Date.now;
    this.#timeoutMs = options.timeoutMs ?? REQUEST_TIMEOUT_MS;
  }

  /** Never throws: anything unexpected becomes an "error" status. */
  async check(): Promise<DaemonStatus> {
    try {
      const status = await this.#check();
      if (status.state !== "running") {
        this.#running = null;
      }
      return status;
    } catch {
      this.#running = null;
      return this.#status({
        state: "error",
        dataDir: this.#dataDir,
        reason: "unexpected",
        message: "Something unexpected went wrong while checking the daemon.",
      });
    }
  }

  async #check(): Promise<DaemonStatus> {
    const dataDir = this.#dataDir;
    if (dataDir === null) {
      return this.#status({
        state: "error",
        dataDir: null,
        reason: "no-data-dir",
        message:
          "There is no home directory to look for the daemon's data directory in.",
      });
    }

    let raw: string;
    try {
      raw = await readFile(path.join(dataDir, DISCOVERY_FILE_NAME), "utf8");
    } catch (error) {
      if (errorCode(error) === "ENOENT") {
        return this.#withoutDiscovery(dataDir);
      }
      return this.#status({
        state: "error",
        dataDir,
        reason: "unreadable",
        message: `${cannotRead(error)} the daemon's discovery file (${DISCOVERY_FILE_NAME}).`,
        detail: errorCode(error),
      });
    }

    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      parsed = null;
    }
    const discovery = parseDiscovery(parsed);
    if (discovery === "invalid-record") {
      return this.#status({
        state: "error",
        dataDir,
        reason: discovery,
        message: `The daemon's discovery file (${DISCOVERY_FILE_NAME}) isn't in the format the daemon writes.`,
      });
    }
    if (discovery === "not-loopback") {
      return this.#status({
        state: "error",
        dataDir,
        reason: discovery,
        message: `The daemon's discovery file (${DISCOVERY_FILE_NAME}) names an address other than 127.0.0.1, so the app won't connect to it.`,
      });
    }

    const [lock, liveness] = await Promise.all([
      this.#readLock(dataDir),
      this.#liveness(discovery.pid, discovery.startedAtMs),
    ]);
    // On Windows the lock can't be read while a daemon holds it (module comment).
    const lockAgrees =
      this.#platform === "win32" || lock?.pid === discovery.pid;
    if (liveness !== "dead" && lockAgrees) {
      return this.#query(dataDir, discovery);
    }
    return (
      (await this.#starting(dataDir, lock, discovery.pid)) ??
      this.#status({
        state: "not-running",
        dataDir,
        stale: true,
        message:
          "No daemon is running. The last one stopped without cleaning up, which is harmless.",
      })
    );
  }

  async #query(dataDir: string, discovery: Discovery): Promise<DaemonStatus> {
    const { address, pid } = discovery;
    const run = `${pid}:${discovery.startedAtMs}`;
    const knownVersion =
      this.#version?.run === run ? this.#version.result : null;
    // Independent calls, so a hung daemon costs one timeout, not two. Both are
    // awaited, so no request outlives the check that made it.
    const [versionCall, healthCall] = await Promise.allSettled([
      knownVersion ??
        this.#call(dataDir, discovery, "version").then(parseVersion),
      this.#call(dataDir, discovery, "health").then(parseHealth),
    ]);
    // Not reaching the daemon, or a token problem, decides the status first.
    for (const call of [versionCall, healthCall]) {
      if (call.status === "rejected" && !(call.reason instanceof RpcError)) {
        return this.#callFailed(dataDir, discovery, call.reason);
      }
    }
    if (versionCall.status === "rejected") {
      return this.#status({
        state: "error",
        dataDir,
        reason: "unexpected",
        message: "The daemon couldn't tell the app which version it is.",
        detail: (versionCall.reason as RpcError).detail,
      });
    }
    const version = versionCall.value;
    this.#version = { run, result: version };

    let health: HealthResult | null = null;
    if (healthCall.status === "fulfilled") {
      health = healthCall.value;
    } else {
      const error = healthCall.reason as RpcError;
      if (error.code !== METHOD_NOT_FOUND) {
        return this.#status({
          state: "unhealthy",
          dataDir,
          address,
          pid,
          daemonVersion: version.daemonVersion,
          message: `The daemon is running on ${address}, but reported a problem when asked for its health.`,
          detail: error.detail,
        });
      }
      // -32601: a daemon older than the method.
    }

    const details: DaemonHealth | null = health && {
      captureContent: health.captureContent,
      lastGlobalPosition: health.lastGlobalPosition,
      erasurePending: health.erasurePending,
      proxy: health.proxy
        ? {
            address: health.proxy.address,
            callsRecorded: health.proxy.callsRecorded,
            recordsDropped: health.proxy.recordsDropped,
          }
        : null,
    };
    this.#running = { dataDir, discovery };
    // A daemon without `health` predates `calls.list` too.
    const calls: RecentCalls = health
      ? await this.#listCalls(dataDir, discovery, null)
      : { state: "unsupported" };
    return this.#status({
      state: "running",
      dataDir,
      address,
      pid,
      daemonVersion: version.daemonVersion,
      schemaVersion: health?.schemaVersion ?? discovery.schemaVersion,
      uptimeMs:
        health?.uptimeMs ?? Math.max(0, this.#now() - discovery.startedAtMs),
      health: details,
      calls,
      message: `The daemon is running and answering on ${address}.`,
    });
  }

  /**
   * The page of calls older than `before`, from the daemon the last check found
   * running. Never throws. Refused when no check has found one running.
   */
  async listCalls(before: number): Promise<RecentCalls> {
    const running = this.#running;
    const notRunning: RecentCalls = {
      state: "failed",
      message: "The daemon isn't running, so older calls can't be loaded.",
    };
    if (running === null) {
      return notRunning;
    }
    try {
      // The token goes only to the process the last check vouched for, so
      // that check's cheap parts run again first: the same discovery record,
      // a lock that agrees, and a live process.
      if (!(await this.#stillRunning(running.dataDir, running.discovery))) {
        if (this.#running === running) {
          this.#running = null;
        }
        return notRunning;
      }
      return await this.#listCalls(running.dataDir, running.discovery, before);
    } catch {
      return {
        state: "failed",
        message: "Something unexpected went wrong while loading calls.",
      };
    }
  }

  /**
   * Forgets the running daemon, so {@link listCalls} refuses until the next
   * check finds one again. Called while the window isn't shown and no checks
   * run, when what the last check found can't be kept up to date.
   */
  forgetRunning(): void {
    this.#running = null;
  }

  /**
   * The discovery file still names the same run, the lock agrees and the
   * process is alive: {@link #check}'s tests without asking the daemon.
   */
  async #stillRunning(dataDir: string, known: Discovery): Promise<boolean> {
    let current: ReturnType<typeof parseDiscovery>;
    try {
      const raw = await readFile(
        path.join(dataDir, DISCOVERY_FILE_NAME),
        "utf8",
      );
      current = parseDiscovery(JSON.parse(raw));
    } catch {
      return false;
    }
    if (
      typeof current === "string" ||
      current.pid !== known.pid ||
      current.port !== known.port ||
      current.startedAtMs !== known.startedAtMs
    ) {
      return false;
    }
    const [lock, liveness] = await Promise.all([
      this.#readLock(dataDir),
      this.#liveness(known.pid, known.startedAtMs),
    ]);
    const lockAgrees = this.#platform === "win32" || lock?.pid === known.pid;
    return liveness !== "dead" && lockAgrees;
  }

  /** One `calls.list` page; every failure becomes "failed" or "unsupported". */
  async #listCalls(
    dataDir: string,
    discovery: Discovery,
    before: number | null,
  ): Promise<RecentCalls> {
    const params: CallsListParams =
      before === null
        ? { limit: LIMITS.pageSize }
        : { limit: LIMITS.pageSize, before };
    let result: unknown;
    try {
      // Within the usual 64 KiB reply cap: the daemon keeps a page's entries
      // to about 48 KiB and says with `nextBefore` that more exist (#63).
      result = await this.#call(dataDir, discovery, "calls.list", params);
    } catch (error) {
      if (error instanceof RpcError && error.code === METHOD_NOT_FOUND) {
        return { state: "unsupported" };
      }
      const detail =
        error instanceof RpcError || error instanceof DaemonCallError
          ? error.detail
          : undefined;
      return {
        state: "failed",
        message: "The daemon didn't return its recent calls.",
        ...(detail === undefined ? {} : { detail }),
      };
    }
    return (
      parseCallsList(result) ?? {
        state: "failed",
        message:
          "The daemon's list of recent calls wasn't in the format the app expects.",
      }
    );
  }

  /** The status for a call that didn't get a JSON-RPC answer. */
  #callFailed(
    dataDir: string,
    { address, pid }: Discovery,
    error: unknown,
  ): DaemonStatus {
    if (error instanceof TokenFileError) {
      return this.#status({
        state: "error",
        dataDir,
        reason: error.reason,
        message: error.message,
        detail: error.detail,
      });
    }
    if (!(error instanceof DaemonCallError)) {
      return this.#status({
        state: "error",
        dataDir,
        reason: "unexpected",
        message: "Something unexpected went wrong while talking to the daemon.",
      });
    }
    const registered = `The daemon (process ${pid}) is registered at ${address}`;
    const unreachable = (message: string): DaemonStatus =>
      this.#status({
        state: "unreachable",
        dataDir,
        address,
        pid,
        message,
        detail: error.detail,
      });
    switch (error.kind) {
      case "unauthorized":
        return this.#status({
          state: "unauthorized",
          dataDir,
          address,
          message:
            "The daemon rejected this app's control token, even after the app re-read the token file.",
        });
      case "refused":
        return unreachable(
          `${registered}, but nothing is accepting connections there.`,
        );
      case "reset":
        return unreachable(
          `${registered}, but it closed the connection without answering.`,
        );
      case "timeout":
        return unreachable(
          `${registered}, but didn't answer within ${formatSeconds(this.#timeoutMs)}.`,
        );
      case "failed":
        return unreachable(
          `${registered}, but the app couldn't connect to it.`,
        );
      case "protocol":
        return this.#status({
          state: "error",
          dataDir,
          reason: "protocol",
          message: `The program at ${address} didn't answer like a Callsheet daemon.`,
          detail: error.detail,
        });
    }
  }

  /** One call; on a 401, re-reads the token file and tries exactly once more. */
  async #call(
    dataDir: string,
    discovery: Discovery,
    method: string,
    params?: object,
  ): Promise<unknown> {
    const id = this.#nextId++;
    const body = JSON.stringify({ jsonrpc: "2.0", method, params, id });
    this.#token ??= await readToken(dataDir);
    let reply = await postRpc(
      discovery.port,
      this.#token,
      body,
      this.#timeoutMs,
    );
    if (reply.status === 401) {
      this.#token = await readToken(dataDir);
      reply = await postRpc(discovery.port, this.#token, body, this.#timeoutMs);
      if (reply.status === 401) {
        throw new DaemonCallError("unauthorized", "The token was rejected.");
      }
    }
    return parseRpcReply(reply, id);
  }

  async #withoutDiscovery(dataDir: string): Promise<DaemonStatus> {
    return (
      (await this.#starting(dataDir, await this.#readLock(dataDir), null)) ??
      this.#status({
        state: "not-running",
        dataDir,
        stale: false,
        message: "No Callsheet daemon is running for this user.",
      })
    );
  }

  /**
   * A daemon that holds the lock but hasn't published `daemon.json` yet: it
   * publishes after opening and migrating the store, which can take a while. (A
   * daemon that is stopping looks the same for a moment, after it removes
   * `daemon.json`; the screen's wording covers both.) The
   * lock's pid counts when it is live and isn't the stale record's own pid, and
   * either Linux confirms it is the process that wrote the lock, or (where the
   * OS can't say) a stale `daemon.json` names another pid or, as a last resort,
   * the lock was written in the last {@link STARTING_GRACE_MS}.
   */
  async #starting(
    dataDir: string,
    lock: LockFile | null,
    stalePid: number | null,
  ): Promise<DaemonStatus | null> {
    if (lock === null || lock.pid === stalePid) {
      return null;
    }
    const liveness = await this.#liveness(lock.pid, lock.writtenAtMs);
    const starting =
      liveness === "verified" ||
      (liveness === "unverified" &&
        (stalePid !== null ||
          this.#now() - lock.writtenAtMs < STARTING_GRACE_MS));
    if (!starting) {
      return null;
    }
    return this.#status({
      state: "starting",
      dataDir,
      pid: lock.pid,
      message: `A daemon (process ${lock.pid}) holds the data directory but hasn't published its address yet.`,
    });
  }

  /**
   * Whether `pid` is still the process that recorded `recordedAtMs` (the
   * daemon's start time, or when it wrote its lock). Dead when the pid is gone;
   * when the record predates this boot (the pid was reused after a reboot); and,
   * on Linux, when the process is gone from `/proc`, has exited but not been
   * reaped (a zombie), or started after the record was written (the pid was
   * reused).
   */
  async #liveness(pid: number, recordedAtMs: number): Promise<Liveness> {
    if (
      !this.#isProcessAlive(pid) ||
      recordedAtMs < this.#bootTimeMs() - BOOT_SLACK_MS
    ) {
      return "dead";
    }
    const info = await this.#processInfo(pid);
    if (info === null) {
      return "unverified";
    }
    if (
      info === "gone" ||
      EXITED_STATES.has(info.state) ||
      info.startedAtMs > recordedAtMs + START_SLACK_MS
    ) {
      return "dead";
    }
    return "verified";
  }

  /**
   * `daemon.lock`'s pid and modification time. Null when it is missing or
   * unreadable, and on Windows, where a running daemon's lock can't be read.
   */
  async #readLock(dataDir: string): Promise<LockFile | null> {
    if (this.#platform === "win32") {
      return null;
    }
    const lockPath = path.join(dataDir, LOCK_FILE_NAME);
    try {
      const [contents, info] = await Promise.all([
        readFile(lockPath, "utf8"),
        stat(lockPath),
      ]);
      const pid = parsePid(contents);
      return pid === null ? null : { pid, writtenAtMs: info.mtimeMs };
    } catch {
      return null;
    }
  }

  #status(
    status: DistributiveOmit<
      DaemonStatus,
      "checkedAtMs" | "customDataDir" | "platform"
    >,
  ): DaemonStatus {
    const { detail, ...rest } = status;
    return {
      ...rest,
      ...(detail === undefined ? {} : { detail }),
      customDataDir: this.#customDataDir,
      platform: commandPlatform(this.#platform),
      checkedAtMs: this.#now(),
    } as DaemonStatus;
  }
}

/** The token file is missing, unreadable or malformed. Never holds the token. */
class TokenFileError extends Error {
  readonly reason: "unreadable" | "bad-token";
  readonly detail: string | undefined;

  constructor(
    reason: TokenFileError["reason"],
    message: string,
    detail?: string,
  ) {
    super(message);
    this.reason = reason;
    this.detail = detail;
  }
}

async function readToken(dataDir: string): Promise<string> {
  let contents: string;
  try {
    contents = await readFile(path.join(dataDir, TOKEN_FILE_NAME), "utf8");
  } catch (error) {
    throw errorCode(error) === "ENOENT"
      ? new TokenFileError(
          "bad-token",
          `The daemon is registered, but its token file (${TOKEN_FILE_NAME}) is missing.`,
        )
      : new TokenFileError(
          "unreadable",
          `${cannotRead(error)} the daemon's token file (${TOKEN_FILE_NAME}).`,
          errorCode(error),
        );
  }
  if (!TOKEN_PATTERN.test(contents)) {
    throw new TokenFileError(
      "bad-token",
      `The daemon's token file (${TOKEN_FILE_NAME}) doesn't hold a Callsheet token.`,
      "Expected 64 lowercase hex characters",
    );
  }
  return contents;
}

function parsePid(contents: string): number | null {
  const pid = Number(contents.trim());
  return /^[0-9]+$/.test(contents.trim()) &&
    Number.isSafeInteger(pid) &&
    pid > 0
    ? pid
    : null;
}

function notTheDaemon(detail: string): DaemonCallError {
  return new DaemonCallError("protocol", "Not a Callsheet daemon.", detail);
}

function parseRpcReply(reply: HttpReply, id: number): unknown {
  if (reply.status !== 200) {
    throw notTheDaemon(`HTTP ${reply.status}`);
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(reply.body);
  } catch {
    throw notTheDaemon("Reply wasn't JSON");
  }
  const response = (parsed ?? {}) as Record<string, unknown>;
  if (response.jsonrpc !== "2.0" || response.id !== id) {
    throw notTheDaemon("Reply wasn't a JSON-RPC 2.0 response");
  }
  const error = response.error as Record<string, unknown> | undefined;
  if (error !== undefined) {
    const code = typeof error?.code === "number" ? error.code : 0;
    const message =
      typeof error?.message === "string" ? error.message.slice(0, 200) : "";
    throw new RpcError(code, message);
  }
  if (!("result" in response)) {
    throw notTheDaemon("Reply had no result");
  }
  return response.result;
}

function parseVersion(result: unknown): VersionResult {
  const daemonVersion = (result as Partial<VersionResult> | null)
    ?.daemonVersion;
  if (typeof daemonVersion !== "string" || daemonVersion.length > 64) {
    throw notTheDaemon("Malformed version reply");
  }
  return { daemonVersion };
}

function parseHealth(result: unknown): HealthResult {
  const health = (result ?? {}) as Record<string, unknown>;
  if (
    health.status !== "ok" ||
    !isCount(health.uptimeMs) ||
    !isCount(health.schemaVersion) ||
    typeof health.captureContent !== "boolean" ||
    !isCount(health.lastGlobalPosition) ||
    typeof health.erasurePending !== "boolean" ||
    !isProxyHealth(health.proxy)
  ) {
    throw notTheDaemon("Malformed health reply");
  }
  return health as HealthResult;
}

/**
 * Absent (no proxy), or a proxy on `127.0.0.1:<non-zero port>`. The address
 * becomes a command the user runs, so nothing else is accepted.
 */
function isProxyHealth(value: unknown): value is ProxyHealth | undefined {
  if (value === undefined || value === null) {
    return true;
  }
  const proxy = value as Record<string, unknown>;
  if (
    typeof proxy !== "object" ||
    typeof proxy.address !== "string" ||
    !isCount(proxy.callsRecorded) ||
    !isCount(proxy.recordsDropped)
  ) {
    return false;
  }
  const digits = ADDRESS_PATTERN.exec(proxy.address)?.[1] ?? "0";
  const port = Number(digits);
  return port >= 1 && port <= 65_535 && String(port) === digits;
}

function errorCode(error: unknown): string | undefined {
  const code = (error as { code?: unknown } | null)?.code;
  return typeof code === "string" ? code : undefined;
}

/** "The app isn't allowed to read" for a permission error, else "couldn't read". */
function cannotRead(error: unknown): string {
  const code = errorCode(error);
  return code === "EACCES" || code === "EPERM"
    ? "The app isn't allowed to read"
    : "The app couldn't read";
}

function formatSeconds(ms: number): string {
  const seconds = ms / 1000;
  return `${seconds} ${seconds === 1 ? "second" : "seconds"}`;
}
