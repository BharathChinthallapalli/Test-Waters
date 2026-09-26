import assert from "node:assert/strict";
import { test } from "node:test";
import { blockNavigation, type NavigableContents } from "./navigation.ts";

function fakeContents() {
  let navigate: ((event: { preventDefault(): void }) => void) | undefined;
  let openWindow: (() => { action: "deny" }) | undefined;
  const contents: NavigableContents = {
    on(_event, listener) {
      navigate = listener;
      return contents;
    },
    setWindowOpenHandler(handler) {
      openWindow = handler;
    },
  };
  blockNavigation(contents);
  return { navigate, openWindow };
}

test("navigating away from the app page is prevented", () => {
  const { navigate } = fakeContents();
  let prevented = false;

  navigate?.({ preventDefault: () => (prevented = true) });

  assert.equal(prevented, true);
});

test("window.open requests are denied", () => {
  const { openWindow } = fakeContents();

  assert.deepEqual(openWindow?.(), { action: "deny" });
});
