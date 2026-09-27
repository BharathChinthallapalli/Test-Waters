import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import {
  isTrustedSender,
  RENDERER_ORIGIN,
  STATUS_CHANGED_CHANNEL,
  STATUS_GET_CHANNEL,
} from "./ipc.ts";

const window = { id: 1 };
const topFrame = { origin: RENDERER_ORIGIN, parent: null };

test("the app window's top app://renderer frame is trusted", () => {
  assert.equal(RENDERER_ORIGIN, "app://renderer");
  assert.equal(
    isTrustedSender({ sender: window, senderFrame: topFrame }, window),
    true,
  );
});

test("other windows, subframes, other origins and gone frames are refused", () => {
  const cases = [
    { sender: { id: 2 }, senderFrame: topFrame },
    { sender: window, senderFrame: { ...topFrame, parent: topFrame } },
    {
      sender: window,
      senderFrame: { origin: "https://example.com", parent: null },
    },
    { sender: window, senderFrame: { origin: "null", parent: null } },
    { sender: window, senderFrame: { origin: "app://other", parent: null } },
    { sender: window, senderFrame: null },
  ];
  for (const event of cases) {
    assert.equal(isTrustedSender(event, window), false, JSON.stringify(event));
  }
  assert.equal(
    isTrustedSender({ sender: null, senderFrame: topFrame }, null),
    false,
  );
});

test("the preload uses the same channel names as the main process", async () => {
  const preload = await readFile(
    new URL("../preload/index.cts", import.meta.url),
    "utf8",
  );
  assert.ok(preload.includes(`"${STATUS_GET_CHANNEL}"`));
  assert.ok(preload.includes(`"${STATUS_CHANGED_CHANNEL}"`));
});

test("the preload never hands the renderer ipcRenderer or an IPC event", async () => {
  const preload = await readFile(
    new URL("../preload/index.cts", import.meta.url),
    "utf8",
  );
  // Listeners get only the status: `listener(status)`, never `listener(event, …)`.
  assert.match(preload, /listener\(status\)/);
  assert.doesNotMatch(preload, /exposeInMainWorld\([^)]*ipcRenderer/);
  assert.match(preload, /Object\.freeze\(\{\s*status: Object\.freeze\(/);
});
