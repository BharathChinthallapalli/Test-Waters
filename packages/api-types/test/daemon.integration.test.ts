// Integration test (feature 02, R2.8, R2.9; feature 03, requirement 7): a
// TypeScript client talks to the built `cs-daemon` binary using only the types
// generated from Rust, and sends one model call through the daemon's proxy to a
// mock upstream on loopback (`--proxy-upstream`), then reads it back with
// `calls.list`.
//
// Needs the binary: `cargo build -p cs-daemon --locked`, then
// `pnpm -C packages/api-types test:integration`. `CS_DAEMON_BIN` overrides the
// default path, target/debug/cs-daemon[.exe] at the repository root. A missing
// binary fails the test; it is never skipped.
import assert from "node:assert/strict";
import { type ChildProcess, spawn } from "node:child_process";
import { access, mkdtemp, readFile, rm } from "node:fs/promises";
import {
  createServer,
  request as httpRequest,
  type IncomingHttpHeaders,
  type Server,
} from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { after, before, test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import type {
  CallsListParams,
  CallsListResult,
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
/** Set when the process could not be started (or signalled). */
let spawnError: Error | undefined;
let discovery: Discovery;

/** A fake API key: it goes to the mock upstream and must appear nowhere else. */
const TEST_API_KEY = "sk-ant-api03-TSINTEGRATIONKEY-not-real";
/** The mock upstream's JSON reply to `POST /v1/messages`. */
const MOCK_MESSAGE = JSON.stringify({
  id: "msg_ts",
  type: "message",
  role: "assistant",
  model: "claude-ts-mock",
  content: [{ type: "text", text: "hello from the mock" }],
  stop_reason: "end_turn",
  usage: { input_tokens: 11, output_tokens: 5 },
});
let upstream: Server;
/** Requests the mock upstream received: path and headers. */
const upstreamSaw: { url: string; headers: IncomingHttpHeaders }[] = [];

/** A mock Anthropic API on loopback: answers every request with MOCK_MESSAGE. */
async function startUpstream(): Promise<string> {
  upstream = createServer((incoming, outgoing) => {
    upstreamSaw.push({ url: incoming.url ?? "", headers: incoming.headers });
    incoming.resume();
    incoming.on("end", () => {
      outgoing.writeHead(200, {
        "content-type": "application/json",
        "request-id": "req_ts_mock",
        "content-length": Buffer.byteLength(MOCK_MESSAGE),
      });
      outgoing.end(MOCK_MESSAGE);
    });
  });
  await new Promise<void>((resolve) =>
    upstream.listen(0, "127.0.0.1", resolve),
  );
  const { port } = upstream.address() as AddressInfo;
  return `http://127.0.0.1:${port}`;
}

/** What one HTTP exchange returned. */
type Reply = { status: number; body: string };

/** Sends a Messages API call to the proxy at `address` (`127.0.0.1:<port>`). */
function proxyCall(address: string, payload: string): Promise<Reply> {
  const separator = address.lastIndexOf(":");
  return new Promise((resolve, reject) => {
    const outgoing = httpRequest(
      {
        // `Host` becomes exactly `127.0.0.1:<port>`, which the proxy requires.
        host: address.slice(0, separator),
        port: Number(address.slice(separator + 1)),
        method: "POST",
        path: "/v1/messages?beta=true",
        agent: false,
        headers: {
          "content-type": "application/json",
          "content-length": Buffer.byteLength(payload),
          "anthropic-version": "2023-06-01",
          "x-api-key": TEST_API_KEY,
          "x-callsheet-run": "ts-integration",
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
    if (spawnError) {
      assert.fail(`cannot start ${DAEMON_BIN}: ${spawnError.message}`);
    }
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
  const upstreamBase = await startUpstream();
  // Default `--listen` is 127.0.0.1:0: the OS picks the port. The proxy picks
  // one from 20000-29999 on this first start and saves it in the data
  // directory.
  daemon = spawn(
    DAEMON_BIN,
    ["--data-dir", dataDir, "--proxy-upstream", upstreamBase],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  // Without a listener, an 'error' (spawn or kill failed) would be thrown.
  daemon.on("error", (error) => {
    spawnError = error;
  });
  daemon.stderr?.setEncoding("utf8");
  daemon.stderr?.on("data", (chunk: string) => {
    stderr += chunk;
  });
  // 'exit' only; a spawn failure is recorded by the 'error' listener above.
  exited = new Promise((resolve) => {
    daemon.once("exit", (code, signal) => resolve([code, signal]));
  });
  discovery = await waitForDiscovery();
});

after(async () => {
  // No pid: the process never started, so there is nothing to wait for.
  if (
    daemon?.pid !== undefined &&
    daemon.exitCode === null &&
    daemon.signalCode === null
  ) {
    daemon.kill("SIGKILL");
    await exited;
  }
  if (root) {
    // Windows may hold the files of a process that just ended for a moment.
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  }
  if (upstream) {
    await new Promise((resolve) => upstream.close(resolve));
  }
});

test("daemon.json names 127.0.0.1, the schema version and the proxy", async () => {
  assert.match(discovery.address, /^127\.0\.0\.1:[1-9]\d*$/);
  assert.equal(typeof discovery.startedAtMs, "number");
  assert.ok(discovery.schemaVersion >= 1);
  assert.ok(discovery.proxyAddress);
  assert.match(discovery.proxyAddress, /^127\.0\.0\.1:[1-9]\d*$/);
  assert.notEqual(discovery.proxyAddress, discovery.address);
  // The port is saved, so the next start reuses it.
  const saved = await readFile(path.join(dataDir, "proxy-port"), "utf8");
  assert.equal(`127.0.0.1:${saved.trim()}`, discovery.proxyAddress);
  // Picked outside every OS's ephemeral port range.
  const port = Number(saved.trim());
  assert.ok(port >= 20000 && port <= 29999, `proxy port ${port}`);
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
  assert.deepEqual(result.proxy, {
    address: discovery.proxyAddress,
    callsRecorded: 0,
    recordsDropped: 0,
  });

  const version = await api.call<VersionResult>("version");
  const cargoToml = await readFile(path.join(REPO_ROOT, "Cargo.toml"), "utf8");
  // The `version` key of the `[workspace.package]` table, wherever it sits in
  // the table and however it is spaced.
  const table = cargoToml
    .split(/^\s*\[workspace\.package\]\s*$/m)[1]
    ?.split(/^\s*\[/m)[0];
  const workspaceVersion = table
    ? /^\s*version\s*=\s*"([^"]+)"/m.exec(table)?.[1]
    : undefined;
  assert.ok(workspaceVersion);
  assert.deepEqual(version.result, { daemonVersion: workspaceVersion });
});

test("a call through the proxy is listed by calls.list", async () => {
  const proxyAddress = discovery.proxyAddress;
  assert.ok(proxyAddress);
  const payload = JSON.stringify({
    model: "claude-ts-mock",
    max_tokens: 16,
    messages: [{ role: "user", content: "hi" }],
  });

  const reply = await proxyCall(proxyAddress, payload);
  assert.equal(reply.status, 200);
  assert.equal(reply.body, MOCK_MESSAGE, "the body passes unchanged");
  // Forwarded with the key and without Callsheet's own run header.
  assert.equal(upstreamSaw.length, 1);
  assert.equal(upstreamSaw[0]?.url, "/v1/messages?beta=true");
  assert.equal(upstreamSaw[0]?.headers["x-api-key"], TEST_API_KEY);
  assert.equal(upstreamSaw[0]?.headers["x-callsheet-run"], undefined);

  // Recording is asynchronous: wait until the call is in the store.
  const api = client(discovery.address);
  const deadline = Date.now() + STARTUP_LIMIT_MS;
  let listed: CallsListResult | undefined;
  for (;;) {
    const params: CallsListParams = { limit: 10 };
    const response = await api.call<CallsListResult, CallsListParams>(
      "calls.list",
      params,
    );
    assert.equal(response.error, undefined);
    listed = response.result;
    if (listed && listed.calls.length > 0) {
      break;
    }
    assert.ok(Date.now() < deadline, "the call was never recorded");
    await delay(50);
  }
  assert.equal(listed.calls.length, 1);
  const entry = listed.calls[0];
  assert.ok(entry);
  assert.equal(entry.runId, "ts-integration");
  const call = entry.call;
  assert.equal(call.provider, "anthropic");
  assert.equal(call.method, "POST");
  assert.equal(call.path, "/v1/messages");
  assert.equal(call.status, 200);
  assert.equal(call.outcome, "completed");
  assert.equal(call.streamed, false);
  assert.equal(call.model, "claude-ts-mock");
  assert.equal(call.requestId, "req_ts_mock");
  assert.equal(call.stopReason, "end_turn");
  assert.deepEqual(call.usage, { inputTokens: 11, outputTokens: 5 });
  assert.equal(call.requestBytes, Buffer.byteLength(payload));
  assert.equal(call.responseBytes, Buffer.byteLength(MOCK_MESSAGE));
  assert.match(call.traceId, /^[0-9a-f]{32}$/);
  assert.equal(listed.nextBefore, undefined);

  const health = await api.call<HealthResult>("health");
  assert.equal(health.result?.proxy?.callsRecorded, 1);
  assert.equal(health.result?.proxy?.recordsDropped, 0);
  assert.ok(!stderr.includes(TEST_API_KEY), "the API key is in the log");
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
