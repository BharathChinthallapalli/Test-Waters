import assert from "node:assert/strict";
import path from "node:path";
import { test } from "node:test";
import { resolveRendererFile } from "./renderer-files.ts";

const root = path.resolve("/app/renderer");

test("the root URL serves index.html as HTML", () => {
  assert.deepEqual(resolveRendererFile("app://renderer/", root), {
    filePath: path.join(root, "index.html"),
    contentType: "text/html; charset=utf-8",
  });
});

test("files inside the renderer directory are served", () => {
  assert.equal(
    resolveRendererFile("app://renderer/style.css", root)?.filePath,
    path.join(root, "style.css"),
  );
});

test("dot segments are normalised by URL parsing and stay inside", () => {
  assert.equal(
    resolveRendererFile("app://renderer/../main/index.js", root)?.filePath,
    path.join(root, "main", "index.js"),
  );
});

test("encoded paths that escape the renderer directory are refused", () => {
  for (const url of [
    "app://renderer/%2e%2e/%2e%2e/etc/passwd",
    "app://renderer/..%2f..%2fetc%2fpasswd",
  ]) {
    assert.equal(resolveRendererFile(url, root), null, url);
  }
});

test("other hosts, schemes and file types are refused", () => {
  assert.equal(resolveRendererFile("app://other/index.html", root), null);
  assert.equal(resolveRendererFile("https://renderer/index.html", root), null);
  assert.equal(resolveRendererFile("app://renderer/notes.txt", root), null);
});
