import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { CONTENT_SECURITY_POLICY } from "./csp.ts";

function directive(name: string): string | undefined {
  return CONTENT_SECURITY_POLICY.split("; ").find((d) =>
    d.startsWith(`${name} `),
  );
}

test("scripts may only come from the app itself", () => {
  assert.equal(directive("default-src"), "default-src 'none'");
  assert.equal(directive("script-src"), "script-src 'self'");
});

test("the policy allows no inline code, eval or remote hosts", () => {
  for (const banned of [
    "'unsafe-inline'",
    "'unsafe-eval'",
    "http:",
    "https:",
    "*",
  ]) {
    assert.ok(!CONTENT_SECURITY_POLICY.includes(banned), banned);
  }
});

test("the renderer's meta tag repeats the header policy exactly", async () => {
  const html = await readFile(
    new URL("../renderer/index.html", import.meta.url),
    "utf8",
  );
  const meta = html.match(
    /http-equiv="Content-Security-Policy"\s+content="([^"]+)"/,
  );

  assert.equal(meta?.[1], CONTENT_SECURITY_POLICY);
});

test("the renderer page has no inline script", async () => {
  const html = await readFile(
    new URL("../renderer/index.html", import.meta.url),
    "utf8",
  );

  assert.doesNotMatch(html, /<script(?![^>]*\ssrc=)[^>]*>/i);
});
