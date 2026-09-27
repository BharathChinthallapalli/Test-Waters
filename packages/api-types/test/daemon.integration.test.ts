// Integration test (feature 02, R2.8, R2.9): a TypeScript client talks to the
// built `cs-daemon` binary using only the types generated from Rust.
//
// Needs the binary: `cargo build -p cs-daemon --locked`, then
// `pnpm -C packages/api-types test:integration`. `CS_DAEMON_BIN` overrides the
// default path, target/debug/cs-daemon[.exe] at the repository root. A missing
// binary fails the test; it is never skipped.
import assert from "node:assert/strict";
import { type ChildProcess, spawn } from "node:child_process";
import { once } from "node:events";
import { access, mkdtemp, readFile, rm } from "node:fs/promises";
import { request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { after, before, test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import type {
  Discovery,
  HealthResult,
  NoParams,
  Request,
  Response,
  TokenRotateResult,
  VersionResult,
} from "../src/index.ts";

const REPO_ROOT = fileURLToPath(new URL("../../../", import.meta.url));
const DAEMON_BIN = path.resolve(
  process.env.CS_DAEMON_BIN ??
    path.join(
      REPO_ROOT,
      "target",
      "debug",
      process.platform === "win32" ? "cs-daemon.exe" : "cs-daemon",
    ),
);
const DISCOVERY_FILE = "daemon.json";
const TOKEN_FILE = "control-token";
/** Generous: CI machines can be slow to start a process. */
const STARTUP_LIMIT_MS = 30_000;
const EXIT_LIMIT_MS = 20_000;

let root: string;
let dataDir: string;
let daemon: ChildProcess;
let exited: Promise<[number | null, NodeJS.Signals | null]>;
let stderr = "";
let discovery: Discovery;

/** What one HTTP exchange returned. */
type Reply = { status: number; body: string };

function post(
  address: string,
  token: string | undefined,
  payload: string,
): Promise<Reply> {
  const separator = address.lastIndexOf(":");
  return new Promise((resolve, reject) => {
    const outgoing = httpRequest(
      {
        // `Host` becomes exactly `127.0.0.1:<port>`; no `Origin` is sent.
        host: address.slice(0, separator),
        port: Number(address.slice(separator + 1)),
        method: "POST",
        path: "/rpc",
        agent: false,
        headers: {
          "content-type": "application/json",
          "content-length": Buffer.byteLength(payload),
          ...(token === undefined ? {} : { authorization: `Bearer ${token}` }),
        },
      },
      (incoming) => {
        const chunks: Buffer[] = [];
        incoming.on("data", (chunk: Buffer) => chunks.push(chunk));
        incoming.on("end", () =>
          resolve({
            status: incoming.statusCode ?? 0,
            body: Buffer.concat(chunks).toString("utf8"),
          }),
        );
        incoming.on("error", reject);
      },
    );
    outgoing.on("error", reject);
    outgoing.end(payload);
  });
}

function readToken(): Promise<string> {
  return readFile(path.join(dataDir, TOKEN_FILE), "utf8");
}

/**
 * A control-API client as ADR 0003 describes it: it reads the token file, and
 * after a 401 re-reads it once and retries once before reporting the failure.
 */
function client(address: string) {
  let token: string | undefined;
  let nextId = 1;
  return {
    async call<R, P = NoParams>(
      method: string,
      params?: P,
    ): Promise<Response<R>> {
      const call: Request<P | NoParams> = {
        jsonrpc: "2.0",
        method,
        params: params ?? {},
        id: nextId++,
      };
      const payload = JSON.stringify(call);
      token ??= await readToken();
      let reply = await post(address, token, payload);
      if (reply.status === 401) {
        token = await readToken();
        reply = await post(address, token, payload);
      }
      assert.equal(reply.status, 200, `HTTP status for ${method}`);
      const response = JSON.parse(reply.body) as Response<R>;
      assert.equal(response.id, call.id);
      return response;
    },
  };
}

async function waitForDiscovery(): Promise<Discovery> {
  const deadline = Date.now() + STARTUP_LIMIT_MS;
  for (;;) {
    if (daemon.exitCode !== null || daemon.signalCode !== null) {
      assert.fail(`the daemon exited during startup: ${stderr}`);
    }
    try {
      const found = JSON.parse(
        await readFile(path.join(dataDir, DISCOVERY_FILE), "utf8"),
      ) as Discovery;
      // A file from an earlier daemon can't be this one's.
      if (found.pid === daemon.pid) {
        return found;
      }
    } catch {
      // Not written yet, or being replaced.
    }
    assert.ok(Date.now() < deadline, `no ${DISCOVERY_FILE}: ${stderr}`);
    await delay(50);
  }
}

before(async () => {
  await access(DAEMON_BIN).catch(() => {
    assert.fail(
      `no daemon binary at ${DAEMON_BIN}: run \`cargo build -p cs-daemon --locked\` or set CS_DAEMON_BIN`,
    );
  });
  root = await mkdtemp(path.join(tmpdir(), "callsheet-it-"));
  dataDir = path.join(root, "data");
  // Default `--listen` is 127.0.0.1:0: the OS picks the port.
  daemon = spawn(DAEMON_BIN, ["--data-dir", dataDir], {
    stdio: ["ignore", "ignore", "pipe"],
  });
  daemon.stderr?.setEncoding("utf8");
  daemon.stderr?.on("data", (chunk: string) => {
    stderr += chunk;
  });
  exited = once(daemon, "exit") as Promise<
    [number | null, NodeJS.Signals | null]
  >;
  discovery = await waitForDiscovery();
});

after(async () => {
  if (daemon && daemon.exitCode === null && daemon.signalCode === null) {
    daemon.kill("SIGKILL");
    await exited;
  }
  if (root) {
    await rm(root, { recursive: true, force: true });
  }
});

test("daemon.json names 127.0.0.1 and the schema version", () => {
  assert.match(discovery.address, /^127\.0\.0\.1:[1-9]\d*$/);
  assert.equal(typeof discovery.startedAtMs, "number");
  assert.ok(discovery.schemaVersion >= 1);
});

test("health and version answer with the generated types", async () => {
  const api = client(discovery.address);

  const health = await api.call<HealthResult>("health");
  assert.equal(health.error, undefined);
  const result: HealthResult | undefined = health.result;
  assert.ok(result);
  assert.equal(result.status, "ok");
  assert.equal(result.schemaVersion, discovery.schemaVersion);
  assert.equal(result.captureContent, false);
  assert.equal(result.lastGlobalPosition, 0);
  assert.equal(result.erasurePending, false);
  assert.ok(result.uptimeMs >= 0);

  const version = await api.call<VersionResult>("version");
  const cargoToml = await readFile(path.join(REPO_ROOT, "Cargo.toml"), "utf8");
  const workspaceVersion =
    /^\[workspace\.package\]\s*\nversion = "([^"]+)"/m.exec(cargoToml)?.[1];
  assert.ok(workspaceVersion);
  assert.deepEqual(version.result, { daemonVersion: workspaceVersion });
});

test("a missing or wrong token gets 401", async () => {
  const payload = JSON.stringify({ jsonrpc: "2.0", method: "version", id: 1 });
  assert.equal((await post(discovery.address, undefined, payload)).status, 401);
  assert.equal(
    (await post(discovery.address, "0".repeat(64), payload)).status,
    401,
  );
});

test("after token.rotate the old token gets 401 and a re-read works", async () => {
  const payload = JSON.stringify({ jsonrpc: "2.0", method: "health", id: 1 });
  const oldToken = await readToken();
  const api = client(discovery.address);

  const rotated = await api.call<TokenRotateResult>("token.rotate");
  assert.deepEqual(rotated.result, {});

  assert.equal((await post(discovery.address, oldToken, payload)).status, 401);
  const newToken = await readToken();
  assert.match(newToken, /^[0-9a-f]{64}$/);
  assert.notEqual(newToken, oldToken);
  assert.equal((await post(discovery.address, newToken, payload)).status, 200);

  // `api` still holds the old token: its next call gets 401, re-reads the file
  // once and succeeds (`call` asserts the final 200).
  const again = await api.call<VersionResult>("version");
  assert.equal(again.error, undefined);
  assert.ok(again.result);
  assert.ok(!stderr.includes(oldToken) && !stderr.includes(newToken));
});

test("the daemon stops and removes daemon.json", async () => {
  if (process.platform === "win32") {
    // No SIGTERM on Windows: this only ends the process.
    daemon.kill();
    await exited;
    return;
  }
  daemon.kill("SIGTERM");
  const [code, signal] = await Promise.race([
    exited,
    delay(EXIT_LIMIT_MS, undefined, { ref: false }).then(() =>
      assert.fail(`the daemon did not exit: ${stderr}`),
    ),
  ]);
  assert.deepEqual([code, signal], [0, null], stderr);
  await assert.rejects(access(path.join(dataDir, DISCOVERY_FILE)), {
    code: "ENOENT",
  });
});
