import assert from "node:assert/strict";
import { test } from "node:test";
import { denyAllPermissions, type PermissionSession } from "./permissions.ts";

function hardenedSession() {
  const handlers: {
    request?: Parameters<PermissionSession["setPermissionRequestHandler"]>[0];
    check?: () => boolean;
    device?: () => boolean;
  } = {};
  denyAllPermissions({
    setPermissionRequestHandler: (handler) => (handlers.request = handler),
    setPermissionCheckHandler: (handler) => (handlers.check = handler),
    setDevicePermissionHandler: (handler) => (handlers.device = handler),
  });
  return handlers;
}

test("permission requests are denied", () => {
  const { request } = hardenedSession();
  let granted: boolean | undefined;

  request?.(null, "media", (answer) => (granted = answer));

  assert.equal(granted, false);
});

test("permission checks are denied", () => {
  assert.equal(hardenedSession().check?.(), false);
});

test("device permissions are denied", () => {
  assert.equal(hardenedSession().device?.(), false);
});
