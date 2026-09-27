// Captures the running app in the light and dark themes, for the design review
// that `.kiro/steering/ui.md` asks for before a UI pull request. Not shipped.
//
// Start the app with a debugging port first, for example under Xvfb:
//   xvfb-run -a -s "-screen 0 1280x800x24" ./node_modules/.bin/electron . \
//     --remote-debugging-port=9333
// (add --no-sandbox only where Chromium's sandbox can't run, such as a root
// container; never in the app itself). Then:
//   node scripts/screenshot.ts <out-prefix> [--wait <ms>] [--size <w>x<h>]
// writes <out-prefix>-light.png and <out-prefix>-dark.png, with reduced motion
// on so the pending mark is still, and prints the page's text.
import { writeFile } from "node:fs/promises";
import { parseArgs } from "node:util";

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    port: { type: "string", default: "9333" },
    wait: { type: "string", default: "4000" },
    size: { type: "string" },
  },
});
const prefix = positionals[0];
if (!prefix) {
  console.error("usage: screenshot.ts <out-prefix> [--wait <ms>] [--size WxH]");
  process.exit(2);
}

interface Target {
  type: string;
  url: string;
  webSocketDebuggerUrl: string;
}

const targets = (await (
  await fetch(`http://127.0.0.1:${values.port}/json/list`)
).json()) as Target[];
const page = targets.find(
  (t) => t.type === "page" && t.url.startsWith("app://"),
);
if (!page) {
  throw new Error("no app:// page is open");
}

const socket = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve) =>
  socket.addEventListener("open", resolve, { once: true }),
);
const waiting = new Map<number, (result: Record<string, unknown>) => void>();
socket.addEventListener("message", (event) => {
  const message = JSON.parse(String(event.data));
  waiting.get(message.id)?.(message.result ?? {});
  waiting.delete(message.id);
});
let nextId = 1;
function send(
  method: string,
  params: Record<string, unknown> = {},
): Promise<Record<string, unknown>> {
  const id = nextId++;
  return new Promise((resolve) => {
    waiting.set(id, resolve);
    socket.send(JSON.stringify({ id, method, params }));
  });
}
const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

if (values.size) {
  const [width, height] = values.size.split("x").map(Number);
  await send("Emulation.setDeviceMetricsOverride", {
    width,
    height,
    deviceScaleFactor: 1,
    mobile: false,
  });
}
await pause(Number(values.wait));
for (const scheme of ["light", "dark"]) {
  await send("Emulation.setEmulatedMedia", {
    features: [
      { name: "prefers-color-scheme", value: scheme },
      { name: "prefers-reduced-motion", value: "reduce" },
    ],
  });
  await pause(300);
  const { data } = await send("Page.captureScreenshot", { format: "png" });
  const file = `${prefix}-${scheme}.png`;
  await writeFile(file, Buffer.from(String(data), "base64"));
  console.log(file);
}
const { result } = await send("Runtime.evaluate", {
  expression: "document.querySelector('main').innerText",
});
console.log((result as { value?: string }).value);

await send("Emulation.setEmulatedMedia", { features: [] });
if (values.size) {
  await send("Emulation.clearDeviceMetricsOverride");
}
socket.close();
