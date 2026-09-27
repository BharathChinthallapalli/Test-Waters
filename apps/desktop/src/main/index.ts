import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow, ipcMain, protocol, session } from "electron";
import { CONTENT_SECURITY_POLICY } from "./csp.ts";
import { DATA_DIR_ENV, DaemonMonitor, resolveDataDir } from "./daemon.ts";
import {
  isTrustedSender,
  RENDERER_ORIGIN,
  STATUS_CHANGED_CHANNEL,
  STATUS_GET_CHANNEL,
} from "./ipc.ts";
import { blockNavigation } from "./navigation.ts";
import { keepSessionOnMachine } from "./network.ts";
import { denyAllPermissions } from "./permissions.ts";
import {
  APP_SCHEME,
  RENDERER_URL,
  resolveRendererFile,
} from "./renderer-files.ts";
import { StatusPoller } from "./status-poller.ts";
import { createWindowOptions } from "./window.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
// The build puts the compiled renderer scripts next to its HTML and CSS.
const rendererDir = path.resolve(here, "../renderer");
const preloadPath = path.resolve(here, "../preload/index.cjs");

// Must run before the app is ready. No bypassCSP privilege: the policy applies.
protocol.registerSchemesAsPrivileged([
  { scheme: APP_SCHEME, privileges: { standard: true, secure: true } },
]);

app.on("web-contents-created", (_event, contents) => {
  blockNavigation(contents);
});

async function serveRendererFile(request: Request): Promise<Response> {
  const file = resolveRendererFile(request.url, rendererDir);
  if (!file) {
    return new Response("Not found", { status: 404 });
  }
  const body = await readFile(file.filePath).catch(() => null);
  if (!body) {
    return new Response("Not found", { status: 404 });
  }
  return new Response(body, {
    headers: {
      "content-type": file.contentType,
      "content-security-policy": CONTENT_SECURITY_POLICY,
      "x-content-type-options": "nosniff",
    },
  });
}

/**
 * Checks the daemon while `window` is visible and not minimised, pushes every
 * result to it, and answers its `status.get` calls. Only the window's own
 * `app://renderer` top frame is answered or sent to.
 */
function watchDaemon(window: BrowserWindow): void {
  const monitor = new DaemonMonitor({
    dataDir: resolveDataDir(process.env, process.platform, homedir()),
    customDataDir: Boolean(process.env[DATA_DIR_ENV]),
  });
  const contents = window.webContents;
  const poller = new StatusPoller({
    check: () => monitor.check(),
    publish: (status) => {
      if (
        !contents.isDestroyed() &&
        contents.mainFrame.origin === RENDERER_ORIGIN
      ) {
        contents.send(STATUS_CHANGED_CHANNEL, status);
      }
    },
  });
  ipcMain.handle(STATUS_GET_CHANNEL, (event) => {
    if (!isTrustedSender(event, contents)) {
      throw new Error("status.get is only available to the Callsheet window");
    }
    return poller.latest;
  });

  const update = (): void =>
    poller.setActive(
      !window.isDestroyed() && window.isVisible() && !window.isMinimized(),
    );
  window.on("show", update);
  window.on("hide", update);
  window.on("minimize", update);
  window.on("restore", update);
  window.on("closed", () => {
    poller.setActive(false);
    ipcMain.removeHandler(STATUS_GET_CHANNEL);
  });
}

async function start(): Promise<void> {
  await app.whenReady();
  denyAllPermissions(session.defaultSession);
  keepSessionOnMachine(session.defaultSession);
  protocol.handle(APP_SCHEME, serveRendererFile);

  const window = new BrowserWindow(createWindowOptions(preloadPath));
  watchDaemon(window);
  window.once("ready-to-show", () => window.show());
  await window.loadURL(RENDERER_URL);
}

app.on("window-all-closed", () => app.quit());

start().catch((error: unknown) => {
  console.error("Callsheet failed to start", error);
  app.exit(1);
});
