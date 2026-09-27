import { readFile, stat } from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import type { HealthResult, VersionResult } from "@callsheet/api-types";
import type { DaemonHealth, DaemonStatus } from "../shared/daemon-status.ts";

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
 * relative one is resolved against the app's working directory). Otherwise it is the daemon's default,
 * which comes from etcetera 0.11.0's native app strategy with the app name
 * `Callsheet` (`src/app_strategy.rs`, `src/{app,base}_strategy/{xdg,apple,windows}.rs`):
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
 * with a non-zero port and its pid is alive (`process.kill(pid, 0)`, which works
 * on Windows too). On Unix the pid must also match the one the daemon wrote into
 * `daemon.lock`, as `cs_daemon::instance::read_discovery` requires; Windows locks
 * that file against reading while the daemon runs, so the check is skipped there.
 *
 * **Residual gap.** `read_discovery` also probes that `daemon.lock` is locked;
 * Node has no file-lock call, so this client can't. After a daemon crash, if the
 * OS reuses its pid for another process of the same user *and* another program
 * binds the old port, the token would be sent to that program. On Windows only
 * the pid is checked, so pid reuse alone is enough there. Closing this needs the
 * lock probe (a native module) or a daemon-side proof of identity; see the
 * follow-ups in docs/progress.md.
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

/** Replies are a few hundred bytes; anything much bigger is not the daemon. */
const MAX_RESPONSE_BYTES = 64 * 1024;

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
 * it exists but belongs to someone else, so it can't be this user's daemon.
 */
export function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** Why a call to the daemon failed, without any detail from the request. */
export class DaemonCallError extends Error {
  readonly kind: "unauthorized" | "refused" | "timeout" | "protocol";

  constructor(kind: DaemonCallError["kind"], message: string) {
    super(message);
    this.name = "DaemonCallError";
    this.kind = kind;
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
              new DaemonCallError("protocol", "The reply was too large."),
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
  return new DaemonCallError("refused", "The connection was refused.");
}

/** Checks the daemon once; see the module comment. */
export interface MonitorOptions {
  dataDir: string | null;
  /** `dataDir` came from `CALLSHEET_DATA_DIR` rather than the default. */
  customDataDir?: boolean;
  platform?: NodeJS.Platform;
  isProcessAlive?: (pid: number) => boolean;
  now?: () => number;
  timeoutMs?: number;
}

/**
 * Checks the daemon and reports a {@link DaemonStatus}. Keeps the control token
 * in memory between checks and re-reads it once after a 401 (R2.9).
 */
export class DaemonMonitor {
  readonly #dataDir: string | null;
  readonly #customDataDir: boolean;
  readonly #platform: NodeJS.Platform;
  readonly #isProcessAlive: (pid: number) => boolean;
  readonly #now: () => number;
  readonly #timeoutMs: number;
  #token: string | null = null;
  #nextId = 1;

  constructor(options: MonitorOptions) {
    this.#dataDir = options.dataDir;
    this.#customDataDir = options.customDataDir ?? false;
    this.#platform = options.platform ?? process.platform;
    this.#isProcessAlive = options.isProcessAlive ?? isProcessAlive;
    this.#now = options.now ?? Date.now;
    this.#timeoutMs = options.timeoutMs ?? REQUEST_TIMEOUT_MS;
  }

  /** Never throws: anything unexpected becomes an "error" status. */
  async check(): Promise<DaemonStatus> {
    try {
      return await this.#check();
    } catch {
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
        message: `Couldn't read ${DISCOVERY_FILE_NAME} in the data directory (${errorCode(error) ?? "unknown error"}).`,
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

    if (
      !this.#isProcessAlive(discovery.pid) ||
      !(await this.#lockNames(dataDir, discovery.pid))
    ) {
      return this.#status({
        state: "not-running",
        dataDir,
        stale: true,
        message:
          "No daemon is running. The last one stopped without cleaning up, which is harmless.",
      });
    }

    return this.#query(dataDir, discovery);
  }

  async #query(dataDir: string, discovery: Discovery): Promise<DaemonStatus> {
    const { address, pid } = discovery;
    let version: VersionResult;
    let health: HealthResult | null;
    try {
      // Independent calls, so a hung daemon costs one timeout, not two. Both
      // are awaited, so no request outlives the check that made it.
      const [versionCall, healthCall] = await Promise.allSettled([
        this.#call(dataDir, discovery, "version").then(parseVersion),
        this.#call(dataDir, discovery, "health").then(parseHealth, (error) => {
          if (error instanceof RpcError && error.code === METHOD_NOT_FOUND) {
            return null; // a daemon older than the method
          }
          throw error;
        }),
      ]);
      if (versionCall.status === "rejected") {
        throw versionCall.reason;
      }
      if (healthCall.status === "rejected") {
        throw healthCall.reason;
      }
      version = versionCall.value;
      health = healthCall.value;
    } catch (error) {
      if (error instanceof TokenFileError) {
        return this.#status({
          state: "error",
          dataDir,
          reason: error.reason,
          message: error.message,
        });
      }
      if (error instanceof DaemonCallError) {
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
            return this.#status({
              state: "unreachable",
              dataDir,
              address,
              pid,
              message: `Process ${pid} is registered at ${address}, but the connection was refused.`,
            });
          case "timeout":
            return this.#status({
              state: "unreachable",
              dataDir,
              address,
              pid,
              message: `Process ${pid} is registered at ${address}, but didn't answer within ${formatSeconds(this.#timeoutMs)}.`,
            });
          case "protocol":
            return this.#status({
              state: "error",
              dataDir,
              reason: "protocol",
              message: `The program at ${address} didn't answer like a Callsheet daemon. ${error.message}`,
            });
        }
      }
      if (error instanceof RpcError) {
        return this.#status({
          state: "error",
          dataDir,
          reason: "unexpected",
          message: `The daemon returned an error (${error.code}): ${error.message}`,
        });
      }
      return this.#status({
        state: "error",
        dataDir,
        reason: "unexpected",
        message: "Something unexpected went wrong while talking to the daemon.",
      });
    }

    const details: DaemonHealth | null = health && {
      captureContent: health.captureContent,
      lastGlobalPosition: health.lastGlobalPosition,
      erasurePending: health.erasurePending,
    };
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
      message: `The daemon is running and answering on ${address}.`,
    });
  }

  /** One call; on a 401, re-reads the token file and tries exactly once more. */
  async #call(
    dataDir: string,
    discovery: Discovery,
    method: string,
  ): Promise<unknown> {
    const id = this.#nextId++;
    const body = JSON.stringify({ jsonrpc: "2.0", method, id });
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
    const pid = await this.#startingPid(dataDir);
    if (pid !== null) {
      return this.#status({
        state: "starting",
        dataDir,
        pid,
        message: `A daemon (process ${pid}) is starting and hasn't published its address yet.`,
      });
    }
    return this.#status({
      state: "not-running",
      dataDir,
      stale: false,
      message: "No Callsheet daemon is running for this user.",
    });
  }

  /**
   * The pid of a daemon that locked the data directory moments ago and hasn't
   * written `daemon.json` yet. Unix only: see the module comment.
   */
  async #startingPid(dataDir: string): Promise<number | null> {
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
      const recent = this.#now() - info.mtimeMs < STARTING_GRACE_MS;
      return pid !== null && recent && this.#isProcessAlive(pid) ? pid : null;
    } catch {
      return null;
    }
  }

  /**
   * On Unix, true when `daemon.lock` holds `pid`, as the daemon writes it. A
   * missing or unreadable lock, or another pid, means the record isn't trusted.
   */
  async #lockNames(dataDir: string, pid: number): Promise<boolean> {
    if (this.#platform === "win32") {
      return true;
    }
    try {
      return (
        parsePid(await readFile(path.join(dataDir, LOCK_FILE_NAME), "utf8")) ===
        pid
      );
    } catch {
      return false;
    }
  }

  #status(
    status: DistributiveOmit<DaemonStatus, "checkedAtMs" | "customDataDir">,
  ): DaemonStatus {
    return {
      ...status,
      customDataDir: this.#customDataDir,
      checkedAtMs: this.#now(),
    } as DaemonStatus;
  }
}

/** The token file is missing, unreadable or malformed. Never holds the token. */
class TokenFileError extends Error {
  readonly reason: "unreadable" | "bad-token";

  constructor(reason: TokenFileError["reason"], message: string) {
    super(message);
    this.reason = reason;
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
          `The daemon is registered, but its ${TOKEN_FILE_NAME} file is missing.`,
        )
      : new TokenFileError(
          "unreadable",
          `Couldn't read the ${TOKEN_FILE_NAME} file (${errorCode(error) ?? "unknown error"}).`,
        );
  }
  if (!TOKEN_PATTERN.test(contents)) {
    throw new TokenFileError(
      "bad-token",
      `The ${TOKEN_FILE_NAME} file isn't a Callsheet token (64 lowercase hex characters).`,
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

function parseRpcReply(reply: HttpReply, id: number): unknown {
  if (reply.status !== 200) {
    throw new DaemonCallError(
      "protocol",
      `It answered with HTTP ${reply.status}.`,
    );
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(reply.body);
  } catch {
    throw new DaemonCallError("protocol", "Its reply wasn't JSON.");
  }
  const response = (parsed ?? {}) as Record<string, unknown>;
  if (response.jsonrpc !== "2.0" || response.id !== id) {
    throw new DaemonCallError(
      "protocol",
      "Its reply wasn't a JSON-RPC 2.0 response.",
    );
  }
  const error = response.error as Record<string, unknown> | undefined;
  if (error !== undefined) {
    const code = typeof error?.code === "number" ? error.code : 0;
    const message =
      typeof error?.message === "string" ? error.message.slice(0, 200) : "";
    throw new RpcError(code, message);
  }
  if (!("result" in response)) {
    throw new DaemonCallError("protocol", "Its reply had no result.");
  }
  return response.result;
}

function parseVersion(result: unknown): VersionResult {
  const daemonVersion = (result as Partial<VersionResult> | null)
    ?.daemonVersion;
  if (typeof daemonVersion !== "string" || daemonVersion.length > 64) {
    throw new DaemonCallError("protocol", "Its version reply was malformed.");
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
    typeof health.erasurePending !== "boolean"
  ) {
    throw new DaemonCallError("protocol", "Its health reply was malformed.");
  }
  return health as HealthResult;
}

function errorCode(error: unknown): string | undefined {
  const code = (error as { code?: unknown } | null)?.code;
  return typeof code === "string" ? code : undefined;
}

function formatSeconds(ms: number): string {
  const seconds = ms / 1000;
  return `${seconds}\u00a0${seconds === 1 ? "second" : "seconds"}`;
}
