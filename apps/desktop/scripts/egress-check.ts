// The idle-start egress gate (issue #56): starts the built desktop app and the
// daemon behind logging proxies that refuse and record every request, and
// under strace, which records every socket they connect or send to. It fails
// if either tries to reach anything beyond this machine. Run by the CI job
// "Desktop makes no outbound requests"; see docs/ci.md. Not shipped: the build
// only compiles src/.
//
// Linux only (process groups, strace, Xvfb). Build the app and cs-daemon
// first, then:
//   xvfb-run -a -s "-screen 0 1280x800x24" node scripts/egress-check.ts
//     [--daemon-bin <path>] [--idle-seconds 20] [--daemon-idle-seconds 10]
//     [--electron-arg <switch>]… [--allow <host:port>]…
//     [--trace-as-user <user>]
// --electron-arg passes a switch to Electron; use `--electron-arg=--no-sandbox`
// only where Chromium's sandbox can't run, such as a root container.
// --trace-as-user runs strace as root through sudo and the traced processes as
// <user>, which Chromium's setuid sandbox needs (see syscall-trace.ts); CI
// passes it. Without it, strace runs as the current user. --allow is
// for a future test that routes a model request on purpose; the idle-start
// check passes none. The daemon binary defaults to `CS_DAEMON_BIN`, then
// `target/debug/cs-daemon` at the repository root.
//
// Order: a canary (scripts/egress-canary.ts) first proves each backstop sees
// what it should; then the app starts with a fresh profile against
// scripts/fake-daemon.ts ("first start"), again with the same profile against
// the real cs-daemon ("restart"), and cs-daemon runs once more on its own
// ("idle start"). Each finding names the process and the phase.
import { type ChildProcess, spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { appendFile, mkdtemp, open, readFile, rm } from "node:fs/promises";
import http from "node:http";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { CANARY } from "./egress-canary-targets.ts";
import {
  type Attempt,
  LoggingProxy,
  partition,
  tally,
} from "./logging-proxy.ts";
import {
  describeDestination,
  type Egress,
  findEgress,
  type TraceAs,
  tracedCommand,
} from "./syscall-trace.ts";

const APP_TITLE = "Callsheet";
/** The status headline once the app has reached the daemon. */
const DAEMON_RUNNING = "Daemon running";
const RENDERER_TIMEOUT_MS = 20_000;
const DEVTOOLS_TIMEOUT_MS = 2000;
const DAEMON_START_TIMEOUT_MS = 10_000;
const CANARY_TIMEOUT_MS = 30_000;
const STOP_TIMEOUT_MS = 10_000;
const POLL_MS = 250;
const LOG_TAIL_LINES = 40;
const CANARY_PHASE = "canary";

const { values } = parseArgs({
  options: {
    "daemon-bin": { type: "string" },
    "idle-seconds": { type: "string", default: "20" },
    "daemon-idle-seconds": { type: "string", default: "10" },
    "electron-arg": { type: "string", multiple: true, default: [] },
    allow: { type: "string", multiple: true, default: [] },
    "trace-as-user": { type: "string" },
  },
});

const appDir = fileURLToPath(new URL("..", import.meta.url));
const repoRoot = path.resolve(appDir, "../..");
const fakeDaemon = fileURLToPath(new URL("fake-daemon.ts", import.meta.url));
const canaryMain = fileURLToPath(new URL("egress-canary.ts", import.meta.url));
const daemonBin =
  values["daemon-bin"] ??
  process.env.CS_DAEMON_BIN ??
  path.join(repoRoot, "target", "debug", "cs-daemon");
const idleMs = seconds(values["idle-seconds"], "--idle-seconds");
const traceAs: TraceAs | null = values["trace-as-user"]
  ? { user: values["trace-as-user"], home: process.env.HOME ?? "/" }
  : null;
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

/** Rejects with `message` after `ms`, for racing against a promise. */
const deadline = (ms: number, message: string) =>
  pause(ms).then(() => {
    throw new Error(message);
  });

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

/**
 * How a child ended; `error` says why there is no exit status: it could not
 * be started, or it did not stop.
 */
interface Exit {
  code: number | null;
  signal: NodeJS.Signals | null;
  error?: string;
}

/** A started child process in its own process group, with output to a file. */
interface Running {
  child: ChildProcess;
  /** Never rejects. */
  exited: Promise<Exit>;
  logFile: string;
  /** Where strace writes the socket calls, when the process is traced. */
  traceFile: string | null;
}

/** Children still running, so an interrupted check can stop them. */
const children = new Set<Running>();

/**
 * Starts `command` in its own process group with output to `logFile`. With a
 * `traceFile`, it runs under strace, which records every socket call of the
 * process and its descendants there.
 */
async function start(
  command: string,
  args: string[],
  env: NodeJS.ProcessEnv,
  logFile: string,
  traceFile: string | null = null,
): Promise<Running> {
  const log = await open(logFile, "w");
  const run = traceFile
    ? tracedCommand(traceFile, command, args, traceAs)
    : { command, args };
  const child = spawn(run.command, run.args, {
    env,
    stdio: ["ignore", log.fd, log.fd],
    detached: true,
  });
  const exited = new Promise<Exit>((resolve) => {
    child.once("error", (error) =>
      resolve({
        code: null,
        signal: null,
        error: `could not start: ${error.message}`,
      }),
    );
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  const running = { child, exited, logFile, traceFile };
  children.add(running);
  exited.finally(() => {
    children.delete(running);
    return log.close();
  });
  return running;
}

/**
 * SIGTERM to the whole process group, then SIGKILL after a grace period.
 * Under --trace-as-user, sudo and strace run as root and this process can't
 * signal them, so the SIGKILL also goes through `sudo kill`; strace's
 * `--kill-on-exit` then ends every tracee, even one that left the group. After
 * a second grace period it gives up, so a stuck process can't hang the check.
 */
async function stop(running: Running): Promise<Exit> {
  const pid = running.child.pid;
  const signal = (name: NodeJS.Signals) => {
    try {
      if (pid) {
        process.kill(-pid, name);
      }
    } catch {
      // Already gone, or only root processes are left in the group.
    }
  };
  const ended = (ms: number) =>
    Promise.race([running.exited, pause(ms).then(() => null)]);
  if (running.child.exitCode === null && running.child.signalCode === null) {
    signal("SIGTERM");
  }
  let exit = await ended(STOP_TIMEOUT_MS);
  if (!exit) {
    signal("SIGKILL");
    if (pid && traceAs && running.traceFile) {
      spawnSync("sudo", [
        "--non-interactive",
        "kill",
        "-KILL",
        "--",
        `-${pid}`,
      ]);
    }
    exit = await ended(STOP_TIMEOUT_MS);
  }
  signal("SIGKILL"); // helpers left in the group
  return (
    exit ?? {
      code: null,
      signal: null,
      error: `did not stop within ${(2 * STOP_TIMEOUT_MS) / 1000} s`,
    }
  );
}

/** "exit code 1", "signal SIGSEGV" or the error, for messages. */
function describeExit({ code, signal, error }: Exit): string {
  if (error) {
    return error;
  }
  return signal ? `signal ${signal}` : `exit code ${code}`;
}

async function logTail(file: string): Promise<string> {
  const text = await readFile(file, "utf8").catch(() => "");
  return text.trimEnd().split("\n").slice(-LOG_TAIL_LINES).join("\n");
}

/** Resolves when `file` exists, or false after `timeoutMs`. */
async function waitForFile(file: string, timeoutMs: number): Promise<boolean> {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
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
      {
        host: "127.0.0.1",
        port,
        path: pathname,
        agent: false,
        timeout: DEVTOOLS_TIMEOUT_MS,
      },
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
 * `--remote-debugging-port=0`. Each step gives up after DEVTOOLS_TIMEOUT_MS.
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
    await Promise.race([
      new Promise((resolve, reject) => {
        socket.addEventListener("open", resolve, { once: true });
        socket.addEventListener("error", reject, { once: true });
      }),
      deadline(DEVTOOLS_TIMEOUT_MS, "the DevTools WebSocket did not open"),
    ]);
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
    const value = await Promise.race([
      reply,
      pause(DEVTOOLS_TIMEOUT_MS).then(() => "null"),
    ]);
    return JSON.parse(value) as PageState | null;
  } finally {
    socket.close();
  }
}

type Component = "desktop app" | "cs-daemon" | "egress check";
const COMPONENTS: readonly Component[] = [
  "desktop app",
  "cs-daemon",
  "egress check",
];

/** A problem that fails the check, attributed to what caused it. */
interface Problem {
  component: Component;
  message: string;
}

/** A socket call from a traced process that reached beyond this machine. */
interface TraceFinding extends Egress {
  component: Component;
  phase: string;
}

const tmp = await mkdtemp(path.join(tmpdir(), "callsheet-egress-"));
const problems: Problem[] = [];
const positives: string[] = [];
const traceFindings: TraceFinding[] = [];

// One proxy per way out, so each request is attributed to what sent it.
const chromium = new LoggingProxy("desktop app: Chromium (--proxy-server)");
const mainNode = new LoggingProxy(
  "desktop app: main-process Node (HTTP(S)_PROXY)",
);
const daemonProxy = new LoggingProxy("cs-daemon (HTTPS_PROXY)");
const proxies = [chromium, mainNode, daemonProxy];

/** Stops every child, closes the proxies and removes the temporary files. */
async function cleanUp(): Promise<void> {
  await Promise.all([...children].map(stop));
  await Promise.all(proxies.map((proxy) => proxy.close()));
  await rm(tmp, { recursive: true, force: true });
}

for (const [signal, code] of [
  ["SIGINT", 130],
  ["SIGTERM", 143],
] as const) {
  process.once(signal, () => {
    console.error(`${signal}: stopping the processes under test`);
    cleanUp().finally(() => process.exit(code));
  });
}

for (const proxy of proxies) {
  await proxy.listen();
}

const electronPath = createRequire(import.meta.url)("electron") as string;

/** The switches the app and the canary both get. */
function electronSwitches(profileDir: string): string[] {
  return [
    `--proxy-server=${chromium.address}`,
    // Removes Chromium's implicit bypass of localhost, loopback and
    // link-local addresses (net/docs/proxy.md), so those go to the proxy too.
    "--proxy-bypass-list=<-loopback>",
    `--user-data-dir=${profileDir}`,
    "--remote-debugging-port=0",
    ...values["electron-arg"],
  ];
}

/** The environment the app and the canary both get. */
function electronEnvironment(dataDir: string): NodeJS.ProcessEnv {
  return environment(mainNode.url, {
    // Node's fetch and global agent in the main process honour the proxy
    // variables only with this set (Node 24). Clients with their own agent,
    // like the daemon client (`agent: false`), still connect directly; the
    // syscall trace sees those.
    NODE_USE_ENV_PROXY: "1",
    CALLSHEET_DATA_DIR: dataDir,
  });
}

const fileLabel = (label: string) => label.replace(/\W+/g, "-");

/** Records what a traced process sent beyond this machine; returns it too. */
async function readTrace(
  running: Running,
  component: Component,
  phase: string,
): Promise<Egress[]> {
  if (!running.traceFile) {
    return [];
  }
  const trace = await readFile(running.traceFile, "utf8").catch(() => null);
  if (trace === null) {
    problems.push({
      component: "egress check",
      message: `${component}, ${phase}: strace wrote no trace`,
    });
    return [];
  }
  const found = findEgress(trace);
  if (phase !== CANARY_PHASE) {
    traceFindings.push(
      ...found.map((egress) => ({ ...egress, component, phase })),
    );
  }
  return found;
}

/**
 * Runs the canary with the app's switches and environment, and checks that
 * each backstop saw what the canary tried. A miss means that backstop no
 * longer works, so the rest of the check would prove nothing.
 */
async function runCanary(): Promise<void> {
  chromium.phase = CANARY_PHASE;
  mainNode.phase = CANARY_PHASE;
  const canary = await start(
    electronPath,
    [canaryMain, ...electronSwitches(path.join(tmp, "canary-profile"))],
    electronEnvironment(path.join(tmp, "canary-data")),
    path.join(tmp, "canary.log"),
    path.join(tmp, "canary.strace"),
  );
  const exit = await Promise.race([
    canary.exited,
    pause(CANARY_TIMEOUT_MS).then(() => null),
  ]);
  await stop(canary);
  const traced = await readTrace(canary, "egress check", CANARY_PHASE);

  const sawRequest = (proxy: LoggingProxy, target: string) =>
    proxy.attempts.some((a) => a.phase === CANARY_PHASE && a.target === target);
  const sawSocket = (host: string, port: number) =>
    traced.some((e) => e.address === host && e.port === port);
  const expectations: [string, boolean, string][] = [
    [
      `main-process fetch reached the HTTPS_PROXY proxy (${CANARY.nodeFetchTarget})`,
      sawRequest(mainNode, CANARY.nodeFetchTarget),
      "NODE_USE_ENV_PROXY no longer routes Node's fetch",
    ],
    [
      `net.fetch reached the --proxy-server proxy (${CANARY.chromiumFetchTarget})`,
      sawRequest(chromium, CANARY.chromiumFetchTarget),
      "--proxy-server no longer applies to the default session",
    ],
    [
      `a partition session's fetch reached the --proxy-server proxy (${CANARY.partitionFetchTarget})`,
      sawRequest(chromium, CANARY.partitionFetchTarget),
      "--proxy-server no longer applies to other sessions",
    ],
    [
      `a link-local request reached the --proxy-server proxy (${CANARY.linkLocalTarget})`,
      sawRequest(chromium, CANARY.linkLocalTarget),
      "<-loopback> no longer removes the implicit bypass rules",
    ],
    [
      `strace saw main-process Node connect to ${CANARY.nodeSocketHost}:${CANARY.nodeSocketPort}`,
      sawSocket(CANARY.nodeSocketHost, CANARY.nodeSocketPort),
      "the syscall trace misses the main process",
    ],
    [
      `strace saw Chromium's network service connect to ${CANARY.directHost}:${CANARY.directPort}`,
      sawSocket(CANARY.directHost, CANARY.directPort),
      "the syscall trace misses Chromium's child processes",
    ],
    [
      `strace saw the DNS lookup of ${CANARY.lookupHost}`,
      traced.some((e) => e.port === 53 || e.reason === "local resolver socket"),
      "the syscall trace misses name resolution",
    ],
  ];
  for (const [what, seen, meaning] of expectations) {
    if (seen) {
      positives.push(`canary: ${what}`);
    } else {
      problems.push({
        component: "egress check",
        message: `canary: expected: ${what}; not seen, so ${meaning}`,
      });
    }
  }
  if (exit === null || exit.code !== 0) {
    problems.push({
      component: "egress check",
      message: `canary: did not finish by itself (${exit ? describeExit(exit) : "timed out"})`,
    });
  }
  if (expectations.some(([, seen]) => !seen) || exit?.code !== 0) {
    console.error("--- canary log (last lines) ---");
    console.error(await logTail(canary.logFile));
  }
}

const profileDir = path.join(tmp, "profile");

async function runApp(phase: string, dataDir: string): Promise<void> {
  chromium.phase = phase;
  mainNode.phase = phase;
  const problemsBefore = problems.length;
  await rm(path.join(profileDir, "DevToolsActivePort"), { force: true });
  const app = await start(
    electronPath,
    [appDir, ...electronSwitches(profileDir)],
    electronEnvironment(dataDir),
    path.join(tmp, `app-${fileLabel(phase)}.log`),
    path.join(tmp, `app-${fileLabel(phase)}.strace`),
  );
  let earlyExit: Exit | null = null;
  app.exited.then((exit) => {
    earlyExit = exit;
  });
  const exitedEarly = () => earlyExit !== null;
  try {
    const end = Date.now() + RENDERER_TIMEOUT_MS;
    let page: PageState | null = null;
    while (!exitedEarly() && Date.now() < end) {
      page = await Promise.race([
        readPage(profileDir),
        deadline(Math.max(0, end - Date.now()), "renderer timeout"),
      ]).catch(() => null);
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
    while (!exitedEarly() && Date.now() < idleUntil) {
      await pause(POLL_MS);
    }
    if (earlyExit) {
      problems.push({
        component: "desktop app",
        message: `${phase}: the app ended before the idle period did (${describeExit(earlyExit)})`,
      });
    }
  } finally {
    await stop(app);
  }
  await readTrace(app, "desktop app", phase);
  if (problems.length > problemsBefore) {
    console.error(`--- desktop app log, ${phase} (last lines) ---`);
    console.error(await logTail(app.logFile));
  }
}

/** A cs-daemon started under strace behind its own proxy. */
interface DaemonRun {
  phase: string;
  running: Running;
  published: boolean;
  earlyExit: () => Exit | null;
}

async function startDaemon(phase: string, dataDir: string): Promise<DaemonRun> {
  daemonProxy.phase = phase;
  const running = await start(
    daemonBin,
    ["--data-dir", dataDir],
    environment(daemonProxy.url),
    path.join(tmp, `daemon-${fileLabel(phase)}.log`),
    path.join(tmp, `daemon-${fileLabel(phase)}.strace`),
  );
  let earlyExit: Exit | null = null;
  running.exited.then((exit) => {
    earlyExit = exit;
  });
  const published = await waitForFile(
    path.join(dataDir, "daemon.json"),
    DAEMON_START_TIMEOUT_MS,
  );
  return { phase, running, published, earlyExit: () => earlyExit };
}

/** Waits `idle` ms (unless it ended), stops it, and checks how it ended. */
async function finishDaemon(run: DaemonRun, idle: number): Promise<void> {
  if (run.published) {
    const idleUntil = Date.now() + idle;
    while (run.earlyExit() === null && Date.now() < idleUntil) {
      await pause(POLL_MS);
    }
  }
  const ended = run.earlyExit();
  const exit = await stop(run.running);
  await readTrace(run.running, "cs-daemon", run.phase);
  if (run.published && ended === null && exit.code === 0) {
    positives.push(
      `cs-daemon, ${run.phase}: published daemon.json, exited 0 on SIGTERM`,
    );
  } else {
    problems.push({
      component: "cs-daemon",
      message: `${run.phase}: daemon.json ${run.published ? "published" : "never published"}; ${ended ? "ended by itself" : "stopped with SIGTERM"} (${describeExit(exit)})`,
    });
    console.error(`--- cs-daemon log, ${run.phase} (last lines) ---`);
    console.error(await logTail(run.running.logFile));
  }
}

async function runChecks(): Promise<void> {
  if (!existsSync(path.join(appDir, "dist", "main", "index.js"))) {
    throw new Error("the desktop app isn't built: run `pnpm build` first");
  }
  if (!existsSync(daemonBin)) {
    throw new Error(
      `no daemon binary at ${daemonBin}: run \`cargo build -p cs-daemon --locked\` or pass --daemon-bin`,
    );
  }
  if (spawnSync("strace", ["-V"]).error) {
    throw new Error("strace isn't installed (apt-get install strace)");
  }
  if (
    traceAs &&
    spawnSync("sudo", ["--non-interactive", "true"]).status !== 0
  ) {
    throw new Error("--trace-as-user needs sudo without a password");
  }

  await runCanary();

  // First start: a fresh profile, against the fake daemon.
  const fakeDir = path.join(tmp, "fake-daemon");
  const fake = await start(
    process.execPath,
    [fakeDaemon, "--data-dir", fakeDir],
    environment(null),
    path.join(tmp, "fake-daemon.log"),
  );
  try {
    if (
      !(await waitForFile(
        path.join(fakeDir, "daemon.json"),
        DAEMON_START_TIMEOUT_MS,
      ))
    ) {
      throw new Error(
        `the fake daemon did not start:\n${await logTail(fake.logFile)}`,
      );
    }
    await runApp("first start", fakeDir);
  } finally {
    await stop(fake);
  }

  // Restart: the same profile, against the real daemon, both traced.
  const pairedDir = path.join(tmp, "daemon-paired");
  const paired = await startDaemon("restart", pairedDir);
  if (paired.published) {
    await runApp("restart", pairedDir);
  }
  await finishDaemon(paired, 0);

  // Idle start: the daemon on its own, with no client.
  await finishDaemon(
    await startDaemon("idle start", path.join(tmp, "daemon-alone")),
    daemonIdleMs,
  );
}

try {
  await runChecks();
} finally {
  for (const proxy of proxies) {
    for (const error of proxy.errors) {
      problems.push({
        component: "egress check",
        message: `the ${proxy.process} proxy failed: ${error}`,
      });
    }
  }
  await cleanUp();
}

// The canary's own requests are its evidence, not failures.
const attempts: Attempt[] = proxies
  .flatMap((proxy) => proxy.attempts)
  .filter((attempt) => attempt.phase !== CANARY_PHASE);
const { allowed, refused } = partition(attempts, values.allow);
const componentOf = (attempt: Attempt): Component =>
  attempt.process.startsWith("cs-daemon") ? "cs-daemon" : "desktop app";

/** Groups trace findings by component, phase, thread name and destination. */
function tallyTrace(findings: readonly TraceFinding[]) {
  const groups = new Map<
    string,
    { finding: TraceFinding; threadName: string; count: number }
  >();
  for (const finding of findings) {
    const threadName = /<([^>]*)>/.exec(finding.thread)?.[1] ?? finding.thread;
    const key = JSON.stringify([
      finding.component,
      finding.phase,
      threadName,
      describeDestination(finding),
      finding.reason,
    ]);
    const group = groups.get(key);
    if (group) {
      group.count += 1;
    } else {
      groups.set(key, { finding, threadName, count: 1 });
    }
  }
  return [...groups.values()];
}

const report: string[] = [];
const summary: string[] = ["## Egress check", ""];
const out = (line: string) => report.push(line);

out("Positive checks:");
for (const line of positives) {
  out(`  ok  ${line}`);
}
out(
  `Outbound requests at the proxies (every one refused with 403, none forwarded): ${refused.length}`,
);
for (const { process, phase, target, count } of tally(refused)) {
  out(`  FAIL  ${process}, ${phase}: ${target} x${count}`);
  summary.push(`- FAIL ${process}, ${phase}: \`${target}\` x${count}`);
}
for (const { process, phase, target, count } of tally(allowed)) {
  out(`  allowed  ${process}, ${phase}: ${target} x${count}`);
}
out(
  `Socket calls beyond this machine in the syscall trace: ${traceFindings.length}`,
);
for (const { finding, threadName, count } of tallyTrace(traceFindings)) {
  const line = `${finding.component} (strace), ${finding.phase}: ${finding.syscall} ${describeDestination(finding)} by thread "${threadName}" (${finding.reason}) x${count}`;
  out(`  FAIL  ${line}`);
  summary.push(`- FAIL ${line}`);
}
for (const { component, message } of problems) {
  out(`  FAIL  ${component}: ${message}`);
  summary.push(`- FAIL ${component}: ${message}`);
}
let failed = false;
const verdicts: string[] = [];
for (const component of COMPONENTS) {
  const requests = refused.filter((a) => componentOf(a) === component).length;
  const traced = traceFindings.filter((f) => f.component === component).length;
  const other = problems.filter((p) => p.component === component).length;
  const passed = requests + traced + other === 0;
  failed ||= !passed;
  out(
    `${component}: ${passed ? "PASS" : "FAIL"} (${requests} proxied requests, ${traced} traced socket calls, ${other} other problems)`,
  );
  verdicts.push(
    `| ${component} | ${passed ? "PASS" : "FAIL"} | ${requests} | ${traced} | ${other} |`,
  );
}
console.log(report.join("\n"));

if (process.env.GITHUB_STEP_SUMMARY) {
  const table = [
    "| Component | Result | Proxied requests | Traced socket calls | Other problems |",
    "| --- | --- | --- | --- | --- |",
    ...verdicts,
  ];
  const checks = positives.map((line) => `- ok ${line}`);
  await appendFile(
    process.env.GITHUB_STEP_SUMMARY,
    `${[...summary.slice(0, 2), ...table, "", ...checks, ...summary.slice(2)].join("\n")}\n`,
  );
}
process.exitCode = failed ? 1 : 0;
