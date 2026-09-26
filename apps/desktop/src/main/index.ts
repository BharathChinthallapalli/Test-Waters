import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow, protocol, session } from "electron";
import { CONTENT_SECURITY_POLICY } from "./csp.ts";
import { blockNavigation } from "./navigation.ts";
import {
  APP_SCHEME,
  RENDERER_URL,
  resolveRendererFile,
} from "./renderer-files.ts";
import { createWindowOptions } from "./window.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const rendererDir = path.resolve(here, "../../src/renderer");
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

async function start(): Promise<void> {
  await app.whenReady();
  session.defaultSession.setPermissionRequestHandler(
    (_contents, _permission, callback) => callback(false),
  );
  protocol.handle(APP_SCHEME, serveRendererFile);

  const window = new BrowserWindow(createWindowOptions(preloadPath));
  window.once("ready-to-show", () => window.show());
  await window.loadURL(RENDERER_URL);
}

app.on("window-all-closed", () => app.quit());

start().catch((error: unknown) => {
  console.error("Callsheet failed to start", error);
  app.exit(1);
});
