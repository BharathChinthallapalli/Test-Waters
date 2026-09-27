// The idle-start egress gate (issue #56): starts the built desktop app and the
// daemon behind logging proxies that refuse and record every request, and
// fails if either tries to reach anything. Run by the CI job "Desktop makes no
// outbound requests"; see docs/ci.md. Not shipped: the build only compiles src/.
//
// Linux only (process groups, Xvfb). Build the app and cs-daemon first, then:
//   xvfb-run -a -s "-screen 0 1280x800x24" node scripts/egress-check.ts
//     [--daemon-bin <path>] [--idle-seconds 20] [--daemon-idle-seconds 10]
//     [--electron-arg <switch>]… [--allow <host:port>]…
// --electron-arg passes a switch to Electron; use `--electron-arg=--no-sandbox`
// only where Chromium's sandbox can't run, such as a root container. --allow is
// for a future test that routes a model request on purpose; the idle-start
// check passes none. The daemon binary defaults to `CS_DAEMON_BIN`, then
// `target/debug/cs-daemon` at the repository root.
//
// The desktop app runs twice with a fresh profile (first start, then restart
// with the same profile) against scripts/fake-daemon.ts on loopback; cs-daemon
// runs once on its own, so a failure names which of the two made the request.
import { type ChildProcess, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, open, readFile, rm } from "node:fs/promises";
import http from "node:http";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import {
  type Attempt,
  LoggingProxy,
  partition,
  tally,
} from "./logging-proxy.ts";

const APP_TITLE = "Callsheet";
/** The status headline once the app has reached the (fake) daemon. */
const DAEMON_RUNNING = "Daemon running";
const RENDERER_TIMEOUT_MS = 20_000;
const DAEMON_START_TIMEOUT_MS = 10_000;
const STOP_TIMEOUT_MS = 10_000;
const POLL_MS = 250;
const LOG_TAIL_LINES = 40;

const { values } = parseArgs({
  options: {
    "daemon-bin": { type: "string" },
    "idle-seconds": { type: "string", default: "20" },
    "daemon-idle-seconds": { type: "string", default: "10" },
    "electron-arg": { type: "string", multiple: true, default: [] },
    allow: { type: "string", multiple: true, default: [] },
  },
});

const appDir = fileURLToPath(new URL("..", import.meta.url));
const repoRoot = path.resolve(appDir, "../..");
const fakeDaemon = fileURLToPath(new URL("fake-daemon.ts", import.meta.url));
const daemonBin =
  values["daemon-bin"] ??
  process.env.CS_DAEMON_BIN ??
  path.join(repoRoot, "target", "debug", "cs-daemon");
const idleMs = seconds(values["idle-seconds"], "--idle-seconds");
const daemonIdleMs = seconds(
  values["daemon-idle-seconds"],
  "--daemon-idle-seconds",
);

function seconds(value: string, flag: string): number {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    console.error(`${flag} must be a positive number of seconds`);
    process.exit(2);
  }
  return parsed * 1000;
}

const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * The environment for a process: the caller's without its proxy variables.
 * For a process under test, every proxy variable is then set to `proxyUrl`
 * with an empty NO_PROXY, so even loopback requests from a client that honours
 * them reach the logging proxy.
 */
function environment(
  proxyUrl: string | null,
  extra: Record<string, string> = {},
): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const [name, value] of Object.entries(process.env)) {
    if (!/proxy/i.test(name) && name !== "ELECTRON_RUN_AS_NODE") {
      env[name] = value;
    }
  }
  if (proxyUrl) {
    for (const name of ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"]) {
      env[name] = proxyUrl;
      env[name.toLowerCase()] = proxyUrl;
    }
    env.NO_PROXY = "";
    env.no_proxy = "";
  }
  return { ...env, ...extra };
}

/** A started child process in its own process group, with output to a file. */
interface Running {
  child: ChildProcess;
  exited: Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
  logFile: string;
}

async function start(
  command: string,
  args: string[],
  env: NodeJS.ProcessEnv,
  logFile: string,
): Promise<Running> {
  const log = await open(logFile, "w");
  const child = spawn(command, args, {
    env,
    stdio: ["ignore", log.fd, log.fd],
    detached: true,
  });
  const exited = new Promise<{
    code: number | null;
    signal: NodeJS.Signals | null;
  }>((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  exited.catch(() => {}).finally(() => log.close());
  return { child, exited, logFile };
}

/** SIGTERM to the whole process group, then SIGKILL after a grace period. */
async function stop(running: Running): Promise<number | null> {
  const signal = (name: NodeJS.Signals) => {
    try {
      if (running.child.pid) {
        process.kill(-running.child.pid, name);
      }
    } catch {
      // Already gone.
    }
  };
  if (running.child.exitCode === null && running.child.signalCode === null) {
    signal("SIGTERM");
  }
  const timer = setTimeout(() => signal("SIGKILL"), STOP_TIMEOUT_MS);
  const { code } = await running.exited;
  clearTimeout(timer);
  signal("SIGKILL"); // helpers left in the group
  return code;
}

async function logTail(file: string): Promise<string> {
  const text = await readFile(file, "utf8").catch(() => "");
  return text.trimEnd().split("\n").slice(-LOG_TAIL_LINES).join("\n");
}

/** Resolves when `file` exists, or false after `timeoutMs`. */
async function waitForFile(file: string, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (existsSync(file)) {
      return true;
    }
    await pause(POLL_MS);
  }
  return false;
}

/** GET a JSON document from the loopback DevTools endpoint, never a proxy. */
function getJson(port: number, pathname: string): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const request = http.get(
      { host: "127.0.0.1", port, path: pathname, agent: false, timeout: 2000 },
      (response) => {
        const chunks: Buffer[] = [];
        response.on("data", (chunk: Buffer) => chunks.push(chunk));
        response.on("end", () => {
          try {
            resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
          } catch (error) {
            reject(error);
          }
        });
      },
    );
    request.on("timeout", () => request.destroy(new Error("timed out")));
    request.on("error", reject);
  });
}

interface PageState {
  url: string;
  title: string;
  headline: string | null;
}

/**
 * Reads the app:// page's URL, title and status headline over the DevTools
 * protocol, the way scripts/screenshot.ts does. The port comes from the
 * `DevToolsActivePort` file Chromium writes into the profile for
 * `--remote-debugging-port=0`.
 */
async function readPage(profileDir: string): Promise<PageState | null> {
  const portFile = await readFile(
    path.join(profileDir, "DevToolsActivePort"),
    "utf8",
  ).catch(() => null);
  const port = Number(portFile?.split("\n")[0]);
  if (!Number.isInteger(port) || port <= 0) {
    return null;
  }
  const targets = (await getJson(port, "/json/list").catch(() => [])) as {
    type: string;
    url: string;
    webSocketDebuggerUrl: string;
  }[];
  const page = targets.find(
    (t) => t.type === "page" && t.url.startsWith("app://"),
  );
  if (!page) {
    return null;
  }
  const socket = new WebSocket(page.webSocketDebuggerUrl);
  try {
    await new Promise((resolve, reject) => {
      socket.addEventListener("open", resolve, { once: true });
      socket.addEventListener("error", reject, { once: true });
    });
    const reply = new Promise<string>((resolve) => {
      socket.addEventListener("message", (event) => {
        const message = JSON.parse(String(event.data));
        if (message.id === 1) {
          resolve(String(message.result?.result?.value ?? "null"));
        }
      });
    });
    socket.send(
      JSON.stringify({
        id: 1,
        method: "Runtime.evaluate",
        params: {
          returnByValue: true,
          expression:
            'JSON.stringify({ url: location.href, title: document.title, headline: document.getElementById("headline")?.textContent?.trim() ?? null })',
        },
      }),
    );
    const value = await Promise.race([reply, pause(2000).then(() => "null")]);
    return JSON.parse(value) as PageState | null;
  } finally {
    socket.close();
  }
}

/** A problem that fails the check, attributed to the desktop app or the daemon. */
interface Problem {
  component: "desktop app" | "cs-daemon";
  message: string;
}

const tmp = await mkdtemp(path.join(tmpdir(), "callsheet-egress-"));
const problems: Problem[] = [];
const positives: string[] = [];

// One proxy per way out, so each request is attributed to what sent it.
const chromium = new LoggingProxy("desktop app: Chromium (--proxy-server)");
const mainNode = new LoggingProxy(
  "desktop app: main-process Node (HTTP(S)_PROXY)",
);
const daemonProxy = new LoggingProxy("cs-daemon (HTTPS_PROXY)");
const proxies = [chromium, mainNode, daemonProxy];
for (const proxy of proxies) {
  await proxy.listen();
}

const electronPath = createRequire(import.meta.url)("electron") as string;
const appEnv = environment(mainNode.url, {
  // Node's fetch and global agent in the main process honour the proxy
  // variables only with this set (Node 24). Clients with their own agent,
  // like the daemon client (`agent: false`), still connect directly.
  NODE_USE_ENV_PROXY: "1",
  CALLSHEET_DATA_DIR: path.join(tmp, "fake-daemon"),
});
const profileDir = path.join(tmp, "profile");

async function runApp(phase: string): Promise<void> {
  chromium.phase = phase;
  mainNode.phase = phase;
  const problemsBefore = problems.length;
  await rm(path.join(profileDir, "DevToolsActivePort"), { force: true });
  const app = await start(
    electronPath,
    [
      appDir,
      `--proxy-server=${chromium.address}`,
      `--user-data-dir=${profileDir}`,
      "--remote-debugging-port=0",
      ...values["electron-arg"],
    ],
    appEnv,
    path.join(tmp, `app-${phase.replace(/\W+/g, "-")}.log`),
  );
  let exitedEarly = false;
  app.exited.then(() => {
    exitedEarly = true;
  });
  try {
    const deadline = Date.now() + RENDERER_TIMEOUT_MS;
    let page: PageState | null = null;
    while (!exitedEarly && Date.now() < deadline) {
      page = await readPage(profileDir).catch(() => null);
      if (page?.title === APP_TITLE && page.headline === DAEMON_RUNNING) {
        break;
      }
      await pause(POLL_MS);
    }
    if (page?.title === APP_TITLE && page.headline === DAEMON_RUNNING) {
      positives.push(
        `desktop app, ${phase}: ${page.url} open, title "${page.title}", status "${page.headline}"`,
      );
    } else {
      problems.push({
        component: "desktop app",
        message: `${phase}: the app:// renderer did not show "${APP_TITLE}" with "${DAEMON_RUNNING}" (saw ${JSON.stringify(page)})`,
      });
    }
    const idleUntil = Date.now() + idleMs;
    while (!exitedEarly && Date.now() < idleUntil) {
      await pause(POLL_MS);
    }
    if (exitedEarly) {
      problems.push({
        component: "desktop app",
        message: `${phase}: the app exited before the idle period ended`,
      });
    }
  } finally {
    await stop(app);
  }
  if (problems.length > problemsBefore) {
    console.error(`--- desktop app log, ${phase} (last lines) ---`);
    console.error(await logTail(app.logFile));
  }
}

async function runDaemon(phase: string): Promise<void> {
  daemonProxy.phase = phase;
  const dataDir = path.join(tmp, "daemon");
  const daemon = await start(
    daemonBin,
    ["--data-dir", dataDir],
    environment(daemonProxy.url),
    path.join(tmp, "daemon.log"),
  );
  daemon.exited.catch(() => {});
  const started = Date.now();
  const published = await waitForFile(
    path.join(dataDir, "daemon.json"),
    DAEMON_START_TIMEOUT_MS,
  );
  await pause(Math.max(0, started + daemonIdleMs - Date.now()));
  const exitedEarly = daemon.child.exitCode !== null;
  const code = await stop(daemon).catch(() => null);
  if (published && !exitedEarly && code === 0) {
    positives.push(
      `cs-daemon, ${phase}: published daemon.json, exited 0 on SIGTERM`,
    );
  } else {
    problems.push({
      component: "cs-daemon",
      message: `${phase}: daemon.json ${published ? "published" : "never published"}, ${exitedEarly ? "exited before SIGTERM" : "stopped by SIGTERM"} with exit code ${code}`,
    });
    console.error(`--- cs-daemon log, ${phase} (last lines) ---`);
    console.error(await logTail(daemon.logFile));
  }
}

try {
  if (!existsSync(path.join(appDir, "dist", "main", "index.js"))) {
    throw new Error("the desktop app isn't built: run `pnpm build` first");
  }
  if (!existsSync(daemonBin)) {
    throw new Error(
      `no daemon binary at ${daemonBin}: run \`cargo build -p cs-daemon --locked\` or pass --daemon-bin`,
    );
  }
  const fake = await start(
    process.execPath,
    [fakeDaemon, "--data-dir", path.join(tmp, "fake-daemon")],
    environment(null),
    path.join(tmp, "fake-daemon.log"),
  );
  try {
    if (
      !(await waitForFile(
        path.join(tmp, "fake-daemon", "daemon.json"),
        DAEMON_START_TIMEOUT_MS,
      ))
    ) {
      throw new Error(
        `the fake daemon did not start:\n${await logTail(fake.logFile)}`,
      );
    }
    await runApp("first start");
    await runApp("restart");
  } finally {
    await stop(fake);
  }
  await runDaemon("idle start");
} finally {
  for (const proxy of proxies) {
    await proxy.close();
  }
  await rm(tmp, { recursive: true, force: true });
}

const attempts: Attempt[] = proxies.flatMap((proxy) => proxy.attempts);
const { allowed, refused } = partition(attempts, values.allow);
const componentOf = (attempt: Attempt): Problem["component"] =>
  attempt.process.startsWith("cs-daemon") ? "cs-daemon" : "desktop app";

console.log("Positive checks:");
for (const line of positives) {
  console.log(`  ok  ${line}`);
}
console.log(
  `Outbound requests (every one refused with 403, none forwarded): ${refused.length}`,
);
for (const { process, phase, target, count } of tally(refused)) {
  console.log(`  FAIL  ${process}, ${phase}: ${target} x${count}`);
}
for (const { process, phase, target, count } of tally(allowed)) {
  console.log(`  allowed  ${process}, ${phase}: ${target} x${count}`);
}
for (const { component, message } of problems) {
  console.log(`  FAIL  ${component}: ${message}`);
}
let failed = false;
for (const component of ["desktop app", "cs-daemon"] as const) {
  const requests = refused.filter((a) => componentOf(a) === component).length;
  const other = problems.filter((p) => p.component === component).length;
  failed ||= requests + other > 0;
  console.log(
    `${component}: ${requests + other === 0 ? "PASS" : "FAIL"} (${requests} outbound requests, ${other} other problems)`,
  );
}
process.exitCode = failed ? 1 : 0;
