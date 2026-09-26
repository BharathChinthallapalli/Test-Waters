import assert from "node:assert/strict";
import { test } from "node:test";
import { createWindowOptions } from "./window.ts";

test("every window gets the hardened web preferences", () => {
  const { webPreferences } = createWindowOptions("/app/preload/index.cjs");

  assert.deepEqual(webPreferences, {
    preload: "/app/preload/index.cjs",
    contextIsolation: true,
    sandbox: true,
    nodeIntegration: false,
    nodeIntegrationInWorker: false,
    nodeIntegrationInSubFrames: false,
    webSecurity: true,
    allowRunningInsecureContent: false,
    experimentalFeatures: false,
    webviewTag: false,
    navigateOnDragDrop: false,
  });
});

test("windows stay hidden until their first paint", () => {
  assert.equal(createWindowOptions("/p.cjs").show, false);
});
