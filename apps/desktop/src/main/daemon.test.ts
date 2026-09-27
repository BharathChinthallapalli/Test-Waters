import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, utimes, writeFile } from "node:fs/promises";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import {
  after,
  afterEach,
  before,
  beforeEach,
  describe,
  test,
} from "node:test";
import type { DaemonStatus } from "../shared/daemon-status.ts";
import {
  DaemonMonitor,
  type MonitorOptions,
  parseDiscovery,
  resolveDataDir,
} from "./daemon.ts";

const TOKEN_A = "a".repeat(64);
const TOKEN_B = "b".repeat(64);
const LIVE_PID = 4242;
const HOUR = 3_600_000;

describe("resolveDataDir", () => {
  test("CALLSHEET_DATA_DIR wins on every platform", () => {
    const env = { CALLSHEET_DATA_DIR: "/srv/cs", XDG_DATA_HOME: "/x" };
    assert.equal(resolveDataDir(env, "linux", "/home/ann"), "/srv/cs");
    assert.equal(resolveDataDir(env, "darwin", "/Users/ann"), "/srv/cs");
  });

  test("Linux uses $XDG_DATA_HOME/callsheet when it is absolute", () => {
    assert.equal(
      resolveDataDir({ XDG_DATA_HOME: "/data" }, "linux", "/home/ann"),
      "/data/callsheet",
    );
  });

  test("Linux ignores a relative XDG_DATA_HOME, as etcetera does", () => {
    assert.equal(
      resolveDataDir({ XDG_DATA_HOME: "rel/data" }, "linux", "/home/ann"),
      "/home/ann/.local/share/callsheet",
    );
    assert.equal(
      resolveDataDir({}, "freebsd", "/home/ann"),
      "/home/ann/.local/share/callsheet",
    );
  });

  test("macOS uses Application Support/Callsheet", () => {
    assert.equal(
      resolveDataDir({}, "darwin", "/Users/ann"),
      "/Users/ann/Library/Application Support/Callsheet",
    );
  });

  test("Windows uses %APPDATA%\\Callsheet\\data, else the home Roaming folder", () => {
    assert.equal(
      resolveDataDir(
        { APPDATA: "C:\\Users\\ann\\AppData\\Roaming" },
        "win32",
        "C:\\Users\\ann",
      ),
      "C:\\Users\\ann\\AppData\\Roaming\\Callsheet\\data",
    );
    assert.equal(
      resolveDataDir({ APPDATA: "" }, "win32", "C:\\Users\\ann"),
      "C:\\Users\\ann\\AppData\\Roaming\\Callsheet\\data",
    );
  });

  test("no home directory and no override gives null", () => {
    assert.equal(resolveDataDir({}, "linux", ""), null);
    assert.equal(resolveDataDir({}, "darwin", ""), null);
    assert.equal(resolveDataDir({}, "win32", ""), null);
  });
});

describe("parseDiscovery", () => {
  const record = {
    pid: 7,
    startedAtMs: 1000,
    address: "127.0.0.1:4100",
    schemaVersion: 1,
  };

  test("accepts what the daemon writes", () => {
    assert.deepEqual(parseDiscovery(record), { ...record, port: 4100 });
  });

  test("refuses every address other than 127.0.0.1", () => {
    for (const address of [
      "0.0.0.0:4100",
      "localhost:4100",
      "[::1]:4100",
      "127.0.0.2:4100",
      "10.0.0.1:4100",
      "127.0.0.1",
    ]) {
      assert.equal(
        parseDiscovery({ ...record, address }),
        "not-loopback",
        address,
      );
    }
  });

  test("refuses port 0, out-of-range ports and malformed records", () => {
    for (const value of [
      { ...record, address: "127.0.0.1:0" },
      { ...record, address: "127.0.0.1:65536" },
      { ...record, pid: 0 },
      { ...record, pid: -1 },
      { ...record, pid: "7" },
      { ...record, schemaVersion: 1.5 },
      { ...record, address: 4100 },
      null,
      [],
      "daemon",
    ]) {
      assert.equal(
        parseDiscovery(value),
        "invalid-record",
        JSON.stringify(value),
      );
    }
  });
});

/** A stand-in daemon: records every request and answers with `respond`. */
interface Recorded {
  method: string | undefined;
  url: string | undefined;
  headers: http.IncomingHttpHeaders;
  body: { method: string; id: number };
}

let server: http.Server;
let port: number;
let requests: Recorded[];
let respond: (call: Recorded, reply: http.ServerResponse) => void;
let dataDir: string;

function rpcResult(reply: http.ServerResponse, id: number, result: unknown) {
  reply
    .writeHead(200, { "content-type": "application/json" })
    .end(JSON.stringify({ jsonrpc: "2.0", id, result }));
}

function rpcError(reply: http.ServerResponse, id: number, code: number) {
  reply
    .writeHead(200, { "content-type": "application/json" })
    .end(
      JSON.stringify({ jsonrpc: "2.0", id, error: { code, message: "no" } }),
    );
}

const HEALTH = {
  status: "ok",
  uptimeMs: 2 * HOUR,
  schemaVersion: 3,
  captureContent: false,
  lastGlobalPosition: 12_408,
  erasurePending: false,
};

/** A daemon with `version` and `health` that accepts `token`. */
function healthyDaemon(token = TOKEN_A) {
  return (call: Recorded, reply: http.ServerResponse) => {
    if (call.headers.authorization !== `Bearer ${token}`) {
      reply.writeHead(401, { "www-authenticate": "Bearer" }).end();
    } else if (call.body.method === "version") {
      rpcResult(reply, call.body.id, { daemonVersion: "0.1.0" });
    } else if (call.body.method === "health") {
      rpcResult(reply, call.body.id, HEALTH);
    } else {
      rpcError(reply, call.body.id, -32601);
    }
  };
}

before(async () => {
  server = http.createServer((request, reply) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk: Buffer) => chunks.push(chunk));
    request.on("end", () => {
      const call: Recorded = {
        method: request.method,
        url: request.url,
        headers: request.headers,
        body: JSON.parse(Buffer.concat(chunks).toString("utf8")),
      };
      requests.push(call);
      respond(call, reply);
    });
  });
  await new Promise<void>((resolve) =>
    server.listen(0, "127.0.0.1", () => resolve()),
  );
  port = (server.address() as AddressInfo).port;
});

after(() => {
  server.closeAllConnections();
  server.close();
});

beforeEach(async () => {
  requests = [];
  respond = healthyDaemon();
  dataDir = await mkdtemp(path.join(tmpdir(), "callsheet-desktop-"));
});

async function publish(
  overrides: Partial<{
    pid: number;
    address: string;
    startedAtMs: number;
  }> = {},
  lockPid: number | null = overrides.pid ?? LIVE_PID,
): Promise<void> {
  await writeFile(
    path.join(dataDir, "daemon.json"),
    JSON.stringify({
      pid: LIVE_PID,
      startedAtMs: Date.now() - HOUR,
      address: `127.0.0.1:${port}`,
      schemaVersion: 3,
      ...overrides,
    }),
  );
  if (lockPid !== null) {
    await writeFile(path.join(dataDir, "daemon.lock"), String(lockPid));
  }
}

async function writeToken(token: string): Promise<void> {
  await writeFile(path.join(dataDir, "control-token"), token);
}

function monitor(options: Partial<MonitorOptions> = {}): DaemonMonitor {
  return new DaemonMonitor({
    dataDir,
    platform: "linux",
    isProcessAlive: (pid) => pid === LIVE_PID,
    timeoutMs: 500,
    ...options,
  });
}

/** Checks, and asserts no token ever appears in what the renderer would get. */
async function check(target = monitor()): Promise<DaemonStatus> {
  const status = await target.check();
  const json = JSON.stringify(status);
  assert.ok(!json.includes(TOKEN_A) && !json.includes(TOKEN_B), json);
  return status;
}

describe("DaemonMonitor", () => {
  afterEach(async () => {
    await rm(dataDir, { recursive: true, force: true });
  });

  test("no daemon.json and no fresh lock: not running", async () => {
    const status = await check();
    assert.equal(status.state, "not-running");
    assert.equal(status.state === "not-running" && status.stale, false);
    assert.equal(requests.length, 0);
  });

  test("a daemon.json whose pid is gone is stale: not running, no request", async () => {
    await publish({ pid: 999 });
    await writeToken(TOKEN_A);
    const status = await check();
    assert.equal(status.state, "not-running");
    assert.equal(status.state === "not-running" && status.stale, true);
    assert.equal(requests.length, 0);
  });

  test("on Unix, a lock naming another pid makes daemon.json stale", async () => {
    await publish({}, 1234);
    await writeToken(TOKEN_A);
    assert.equal((await check()).state, "not-running");
    assert.equal(requests.length, 0);
  });

  test("on Unix, a missing or unreadable lock makes daemon.json untrusted", async () => {
    await publish({}, null);
    await writeToken(TOKEN_A);
    assert.equal((await check()).state, "not-running");
    await mkdir(path.join(dataDir, "daemon.lock"));
    assert.equal((await check()).state, "not-running");
    assert.equal(requests.length, 0);
  });

  test("on Windows the lock can't be read, so it isn't checked", async () => {
    await publish({}, null);
    await writeToken(TOKEN_A);
    assert.equal(
      (await check(monitor({ platform: "win32" }))).state,
      "running",
    );
  });

  test("a fresh lock with a live pid and no daemon.json: starting", async () => {
    await writeFile(path.join(dataDir, "daemon.lock"), String(LIVE_PID));
    const status = await check();
    assert.equal(status.state, "starting");
    assert.equal(status.state === "starting" && status.pid, LIVE_PID);
  });

  test("an old lock is not a starting daemon", async () => {
    const lock = path.join(dataDir, "daemon.lock");
    await writeFile(lock, String(LIVE_PID));
    const old = new Date(Date.now() - 60_000);
    await utimes(lock, old, old);
    assert.equal((await check()).state, "not-running");
  });

  test("running: calls version and health on 127.0.0.1 with the token", async () => {
    await publish();
    await writeToken(TOKEN_A);
    const status = await check();

    assert.equal(status.state, "running");
    assert.ok(status.state === "running");
    assert.equal(status.address, `127.0.0.1:${port}`);
    assert.equal(status.daemonVersion, "0.1.0");
    assert.equal(status.schemaVersion, 3);
    assert.equal(status.uptimeMs, 2 * HOUR);
    assert.deepEqual(status.health, {
      captureContent: false,
      lastGlobalPosition: 12_408,
      erasurePending: false,
    });

    assert.deepEqual(requests.map((r) => r.body.method).sort(), [
      "health",
      "version",
    ]);
    for (const request of requests) {
      assert.equal(request.method, "POST");
      assert.equal(request.url, "/rpc");
      assert.equal(request.headers.host, `127.0.0.1:${port}`);
      assert.equal(request.headers.authorization, `Bearer ${TOKEN_A}`);
      assert.equal(request.headers.origin, undefined);
      assert.equal(request.headers["content-type"], "application/json");
    }
  });

  test("a daemon without health (-32601): running, uptime from daemon.json", async () => {
    const startedAtMs = 1_000_000;
    await publish({ startedAtMs });
    await writeToken(TOKEN_A);
    respond = (call, reply) =>
      call.body.method === "version"
        ? rpcResult(reply, call.body.id, { daemonVersion: "0.1.0" })
        : rpcError(reply, call.body.id, -32601);
    const status = await check(monitor({ now: () => startedAtMs + 90_000 }));

    assert.equal(status.state, "running");
    assert.ok(status.state === "running");
    assert.equal(status.health, null);
    assert.equal(status.uptimeMs, 90_000);
    assert.equal(status.schemaVersion, 3);
  });

  test("on 401 the token file is re-read once and the call retried (R2.9)", async () => {
    await publish();
    await writeToken(TOKEN_A);
    const target = monitor();
    assert.equal((await check(target)).state, "running");

    // The token is rotated: the daemon now accepts only TOKEN_B.
    await writeToken(TOKEN_B);
    respond = healthyDaemon(TOKEN_B);
    requests = [];
    assert.equal((await check(target)).state, "running");
    assert.deepEqual(
      requests.map((r) => r.headers.authorization).sort(),
      [TOKEN_A, TOKEN_A, TOKEN_B, TOKEN_B].map((t) => `Bearer ${t}`),
    );
  });

  test("still 401 after re-reading: unauthorized, after exactly two tries per call", async () => {
    await publish();
    await writeToken(TOKEN_A);
    respond = healthyDaemon(TOKEN_B);
    const status = await check();
    assert.equal(status.state, "unauthorized");
    assert.deepEqual(requests.map((r) => r.body.method).sort(), [
      "health",
      "health",
      "version",
      "version",
    ]);
  });

  test("a refused connection: unreachable", async () => {
    const closed = http.createServer();
    await new Promise<void>((resolve) =>
      closed.listen(0, "127.0.0.1", () => resolve()),
    );
    const closedPort = (closed.address() as AddressInfo).port;
    await new Promise((resolve) => closed.close(resolve));
    await publish({ address: `127.0.0.1:${closedPort}` });
    await writeToken(TOKEN_A);

    const status = await check();
    assert.equal(status.state, "unreachable");
    assert.match(status.message, /refused/);
  });

  test("no answer within the timeout: unreachable", async () => {
    await publish();
    await writeToken(TOKEN_A);
    respond = () => {}; // never answers
    const started = Date.now();
    const status = await check(monitor({ timeoutMs: 200 }));
    assert.equal(status.state, "unreachable");
    assert.match(status.message, /didn't answer within 0\.2\sseconds/);
    assert.ok(Date.now() - started < 2000);
  });

  test("a daemon.json naming another host: error, and nothing is sent", async () => {
    await publish({ address: `10.1.2.3:${port}` });
    await writeToken(TOKEN_A);
    const status = await check();
    assert.equal(status.state, "error");
    assert.equal(status.state === "error" && status.reason, "not-loopback");
    assert.equal(requests.length, 0);
  });

  test("unparseable daemon.json: error", async () => {
    await writeFile(path.join(dataDir, "daemon.json"), "{not json");
    const status = await check();
    assert.equal(status.state === "error" && status.reason, "invalid-record");
  });

  test("a malformed or missing token: error, and nothing is sent", async () => {
    await publish();
    const missing = await check();
    assert.equal(missing.state === "error" && missing.reason, "bad-token");

    await writeToken(`${TOKEN_A}\n`);
    const malformed = await check();
    assert.equal(malformed.state === "error" && malformed.reason, "bad-token");
    assert.equal(requests.length, 0);
  });

  test("something that isn't the daemon: error", async () => {
    await publish();
    await writeToken(TOKEN_A);
    respond = (_call, reply) => reply.writeHead(200).end("<html>hello</html>");
    const status = await check();
    assert.equal(status.state === "error" && status.reason, "protocol");
  });

  test("a malformed health reply is not shown as running", async () => {
    await publish();
    await writeToken(TOKEN_A);
    respond = (call, reply) =>
      call.body.method === "version"
        ? rpcResult(reply, call.body.id, { daemonVersion: "0.1.0" })
        : rpcResult(reply, call.body.id, { ...HEALTH, uptimeMs: "long" });
    const status = await check();
    assert.equal(status.state === "error" && status.reason, "protocol");
  });

  test("check never throws; a failure keeps the data directory", async () => {
    await publish();
    await writeToken(TOKEN_A);
    const status = await check(
      monitor({
        customDataDir: true,
        isProcessAlive: () => {
          throw new Error("boom");
        },
      }),
    );
    assert.equal(status.state === "error" && status.reason, "unexpected");
    assert.equal(status.state === "error" && status.dataDir, dataDir);
    assert.equal(status.customDataDir, true);
  });

  test("no data directory: error", async () => {
    const status = await check(monitor({ dataDir: null }));
    assert.equal(status.state === "error" && status.reason, "no-data-dir");
  });

  test("statuses say whether the data directory is custom", async () => {
    const status = await check(monitor({ customDataDir: true }));
    assert.equal(status.customDataDir, true);
    assert.equal((await check()).customDataDir, false);
  });
});
