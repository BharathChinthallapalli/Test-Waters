import assert from "node:assert/strict";
import { test } from "node:test";
import { formatCount, formatDuration, startCommand } from "./format.ts";

const S = 1000;
const MIN = 60 * S;
const H = 60 * MIN;
const D = 24 * H;
const nbsp = (text: string) => text.replaceAll(" ", " ");

test("durations use the two largest units, as people say them", () => {
  const cases: Array<[number, string]> = [
    [0, "0 s"],
    [45 * S + 900, "45 s"],
    [14 * MIN + 59 * S, "14 min"],
    [2 * H, "2 h"],
    [2 * H + 14 * MIN + 30 * S, "2 h 14 min"],
    [3 * D + 4 * H + 59 * MIN, "3 d 4 h"],
    [9 * D, "9 d"],
    [-5, "0 s"],
  ];
  for (const [ms, expected] of cases) {
    // Number and unit are joined by a no-break space; units by a normal one.
    const want = expected.replace(/(\d) /g, "$1 ");
    assert.equal(formatDuration(ms), want, `${ms} ms`);
  }
  assert.equal(
    formatDuration(2 * H + 14 * MIN),
    `${nbsp("2 h")} ${nbsp("14 min")}`,
  );
});

test("counts are grouped", () => {
  assert.equal(formatCount(0, "en-US"), "0");
  assert.equal(formatCount(12_408, "en-US"), "12,408");
  assert.equal(formatCount(1_284_093, "en-US"), "1,284,093");
  assert.equal(formatCount(12_408, "de-DE"), "12.408");
});

test("the start command passes a custom data directory, quoted if needed", () => {
  assert.equal(startCommand(null), "cs-daemon");
  assert.equal(startCommand("/srv/cs"), "cs-daemon --data-dir /srv/cs");
  assert.equal(
    startCommand("C:\\Users\\ann\\cs-data"),
    "cs-daemon --data-dir C:\\Users\\ann\\cs-data",
  );
  assert.equal(
    startCommand("/Users/ann/My Data"),
    "cs-daemon --data-dir '/Users/ann/My Data'",
  );
});

test("nothing in a data directory's name is expanded by the shell", () => {
  assert.equal(
    startCommand("/tmp/a;rm -rf $(x) `y`"),
    "cs-daemon --data-dir '/tmp/a;rm -rf $(x) `y`'",
  );
  assert.equal(
    startCommand("/tmp/ann's"),
    "cs-daemon --data-dir '/tmp/ann'\\''s'",
  );
});
