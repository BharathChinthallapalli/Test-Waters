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
    spellcheck: false,
  });
});

test("windows stay hidden until their first paint", () => {
  assert.equal(createWindowOptions("/p.cjs").show, false);
});

test("windows can't shrink below the status screen's narrow layout", () => {
  const options = createWindowOptions("/p.cjs");
  assert.equal(options.minWidth, 420);
  assert.equal(options.minHeight, 480);
});
