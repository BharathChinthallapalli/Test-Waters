// The egress check's canary (issue #56): an Electron main process that
// `egress-check.ts` starts with exactly the switches and environment it gives
// the app, and that tries to reach the network every way the check claims to
// see. If a proxy or the syscall trace misses one of these, the check fails:
// an Electron or Node change has disabled that backstop, and a pass would mean
// nothing. Not shipped: the build only compiles src/.
//
//   electron scripts/egress-canary.ts <same switches as the app>
//
// Every attempt is refused or can't succeed (see egress-canary-targets.ts).
// It prints one line per attempt and exits.
import { lookup } from "node:dns/promises";
import net from "node:net";
import { app, net as electronNet, session } from "electron";
import { CANARY } from "./egress-canary-targets.ts";

const ATTEMPT_TIMEOUT_MS = 5000;

/** Runs one attempt; an error is the expected outcome and is only reported. */
async function attempt(name: string, run: () => Promise<unknown>) {
  try {
    await run();
    console.log(`canary: ${name}: completed`);
  } catch (error) {
    console.log(`canary: ${name}: ${(error as Error).message}`);
  }
}

function connectOnce(host: string, port: number): Promise<void> {
  return new Promise((resolve) => {
    const socket = net.connect({ host, port });
    const done = () => {
      socket.destroy();
      resolve();
    };
    socket.setTimeout(ATTEMPT_TIMEOUT_MS, done);
    socket.once("connect", done);
    socket.once("error", done);
  });
}

const timeout = () => AbortSignal.timeout(ATTEMPT_TIMEOUT_MS);

/** Tries every way out at once, then quits. */
async function run(): Promise<void> {
  const partition = session.fromPartition("egress-canary");
  const direct = session.fromPartition("egress-canary-direct");
  // A session left alone downloads a spellcheck dictionary; in the direct
  // session that would really leave the machine. Same fix as the app's.
  for (const ses of [session.defaultSession, partition, direct]) {
    ses.setSpellCheckerLanguages([]);
    ses.setSpellCheckerEnabled(false);
  }
  await direct.setProxy({ mode: "direct" });
  await Promise.all([
    attempt("main-process fetch", () =>
      fetch(CANARY.nodeFetchUrl, { signal: timeout() }),
    ),
    attempt("net.fetch, default session", () =>
      electronNet.fetch(CANARY.chromiumFetchUrl, { signal: timeout() }),
    ),
    attempt("fetch, partition session", () =>
      partition.fetch(CANARY.partitionFetchUrl, { signal: timeout() }),
    ),
    attempt("net.fetch, link-local", () =>
      electronNet.fetch(CANARY.linkLocalUrl, { signal: timeout() }),
    ),
    attempt("fetch, direct session", () =>
      direct.fetch(CANARY.directFetchUrl, { signal: timeout() }),
    ),
    attempt("main-process socket", () =>
      connectOnce(CANARY.nodeSocketHost, CANARY.nodeSocketPort),
    ),
    attempt("main-process DNS lookup", () => lookup(CANARY.lookupHost)),
  ]);
}

// Not a top-level `await app.whenReady()`: with one, Electron 44.4.5 never
// became ready here and the canary timed out.
app
  .whenReady()
  .then(run)
  .finally(() => app.quit());
