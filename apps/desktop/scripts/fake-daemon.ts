// A stand-in daemon for rendering the status screen's states by hand. Not part
// of the app and never shipped: the build only compiles src/.
//
//   node scripts/fake-daemon.ts --data-dir <dir> [--mode <mode>] [options]
//
// It writes daemon.json, daemon.lock and control-token into <dir> the way
// cs-daemon does, with its own pid, and serves POST /rpc on 127.0.0.1 with the
// same Host and bearer checks. Modes:
//   healthy        `version` and `health` (default)
//   no-health      `health` answers -32601, like a daemon before task 11
//   unauthorized   accepts no token: every request gets 401
//   unreachable    publishes a port nothing listens on
//   hang           accepts connections and never answers
// Options for `healthy`: --capture on|off, --events <n>, --uptime-ms <n>,
// --erasure-pending.
import { randomBytes } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { parseArgs } from "node:util";

const { values } = parseArgs({
  options: {
    "data-dir": { type: "string" },
    mode: { type: "string", default: "healthy" },
    capture: { type: "string", default: "off" },
    events: { type: "string", default: "12408" },
    "uptime-ms": { type: "string", default: String((2 * 60 + 14) * 60_000) },
    "erasure-pending": { type: "boolean", default: false },
  },
});

const dataDir = values["data-dir"];
if (!dataDir) {
  console.error("usage: fake-daemon.ts --data-dir <dir> [--mode <mode>]");
  process.exit(2);
}
const mode = values.mode;
const token = randomBytes(32).toString("hex");
const startedAtMs = Date.now() - Number(values["uptime-ms"]);

function reply(id: unknown, body: Record<string, unknown>): string {
  return JSON.stringify({ jsonrpc: "2.0", id, ...body });
}

function answer(method: unknown, id: unknown): string {
  switch (method) {
    case "version":
      return reply(id, { result: { daemonVersion: "0.1.0" } });
    case "health":
      if (mode === "no-health") {
        return reply(id, {
          error: { code: -32601, message: "Method not found" },
        });
      }
      return reply(id, {
        result: {
          status: "ok",
          uptimeMs: Date.now() - startedAtMs,
          schemaVersion: 1,
          captureContent: values.capture === "on",
          lastGlobalPosition: Number(values.events),
          erasurePending: values["erasure-pending"],
        },
      });
    default:
      return reply(id, {
        error: { code: -32601, message: "Method not found" },
      });
  }
}

const server = http.createServer((request, response) => {
  if (mode === "hang") {
    return; // never answers
  }
  const port = (server.address() as { port: number }).port;
  if (request.headers.origin || request.headers.host !== `127.0.0.1:${port}`) {
    response.writeHead(403).end();
    return;
  }
  if (
    mode === "unauthorized" ||
    request.headers.authorization !== `Bearer ${token}`
  ) {
    response.writeHead(401, { "www-authenticate": "Bearer" }).end();
    return;
  }
  const chunks: Buffer[] = [];
  request.on("data", (chunk: Buffer) => chunks.push(chunk));
  request.on("end", () => {
    const call = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    response
      .writeHead(200, { "content-type": "application/json" })
      .end(answer(call.method, call.id));
  });
});

server.listen(0, "127.0.0.1", async () => {
  const port = (server.address() as { port: number }).port;
  if (mode === "unreachable") {
    await new Promise((resolve) => server.close(resolve));
    setInterval(() => {}, 60_000); // stay alive so the pid is
  }
  await mkdir(dataDir, { recursive: true, mode: 0o700 });
  const file = (name: string) => path.join(dataDir, name);
  await writeFile(file("control-token"), token, { mode: 0o600 });
  await writeFile(file("daemon.lock"), String(process.pid), { mode: 0o600 });
  await writeFile(
    file("daemon.json"),
    JSON.stringify({
      pid: process.pid,
      startedAtMs,
      address: `127.0.0.1:${port}`,
      schemaVersion: 1,
    }),
    { mode: 0o600 },
  );
  console.log(`fake daemon (${mode}) pid ${process.pid}, ${dataDir}`);
});
