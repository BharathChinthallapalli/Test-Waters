import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
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

const arg = (command: string) => command.replace("cs-daemon --data-dir ", "");

test("the start command passes only a custom data directory", () => {
  assert.equal(startCommand(null, "posix"), "cs-daemon");
  assert.equal(startCommand(null, "windows"), "cs-daemon");
  assert.equal(
    startCommand("/srv/cs", "posix"),
    "cs-daemon --data-dir /srv/cs",
  );
});

test("POSIX: plain paths stay bare, anything else is single-quoted", () => {
  const cases: Array<[string, string]> = [
    ["/srv/cs-data_1/v2.0", "/srv/cs-data_1/v2.0"],
    ["/srv/a,b", "/srv/a,b"], // a comma is plain text in sh
    ["/Users/ann/My Data", "'/Users/ann/My Data'"],
    ["/tmp/ann's", "'/tmp/ann'\\''s'"],
    ['/tmp/say "hi"', `'/tmp/say "hi"'`],
    ["/tmp/back\\slash", "'/tmp/back\\slash'"], // an escape in sh
    ["/tmp/a;rm -rf $(x) `y`", "'/tmp/a;rm -rf $(x) `y`'"],
    ["/tmp/~ann/*", "'/tmp/~ann/*'"],
  ];
  for (const [dir, quoted] of cases) {
    assert.equal(arg(startCommand(dir, "posix")), quoted, dir);
  }
});

test("POSIX: sh reads every quoted path back exactly", {
  skip: process.platform === "win32",
}, () => {
  for (const dir of [
    "/Users/ann/My Data",
    "/tmp/ann's 'quoted'",
    '/tmp/say "hi", \\n $HOME `id` $(id) *',
    "/tmp/tab\there",
  ]) {
    const echoed = execFileSync(
      "/bin/sh",
      ["-c", `printf %s ${arg(startCommand(dir, "posix"))}`],
      { encoding: "utf8" },
    );
    assert.equal(echoed, dir);
  }
});

test("Windows: double quotes, which cmd.exe and PowerShell both take literally", () => {
  const cases: Array<[string, string]> = [
    ["C:\\Users\\ann\\cs-data", "C:\\Users\\ann\\cs-data"],
    ["C:\\Users\\ann\\My Data", '"C:\\Users\\ann\\My Data"'],
    ["C:\\data\\a,b", '"C:\\data\\a,b"'], // the array operator in PowerShell
    ["C:\\data\\@x", '"C:\\data\\@x"'], // splatting in PowerShell
    ["C:\\data\\ann's", `"C:\\data\\ann's"`],
    ["C:\\data\\say \u2018hi\u2019", '"C:\\data\\say \u2018hi\u2019"'],
    ["C:\\data\\a&b|c^d", '"C:\\data\\a&b|c^d"'], // cmd.exe operators
  ];
  for (const [dir, quoted] of cases) {
    assert.equal(arg(startCommand(dir, "windows")), quoted, dir);
  }
});

test("Windows: what double quotes would expand gets PowerShell single quotes", () => {
  const cases: Array<[string, string]> = [
    ["C:\\data\\$env:x `n", "'C:\\data\\$env:x `n'"], // PowerShell
    ["C:\\data\\100%", "'C:\\data\\100%'"], // cmd.exe
    ["C:\\data\\\u201cdq\u201d", "'C:\\data\\\u201cdq\u201d'"], // double quotes
    ["C:\\data\\\u201elow", "'C:\\data\\\u201elow'"],
    ["C:\\My Data\\", "'C:\\My Data\\'"], // \" would escape the quote
    ["C:\\data\\ann's $x", "'C:\\data\\ann''s $x'"],
    [
      "C:\\data\\\u2018q\u2019 \u201alow\u201b $x",
      "'C:\\data\\\u2018\u2018q\u2019\u2019 \u201a\u201alow\u201b\u201b $x'",
    ],
  ];
  for (const [dir, quoted] of cases) {
    assert.equal(arg(startCommand(dir, "windows")), quoted, dir);
  }
});
