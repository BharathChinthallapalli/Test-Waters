import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { test } from "node:test";
import {
  baseUrlCommands,
  formatCallDuration,
  formatCallTime,
  formatCount,
  formatDuration,
  inputTokensBreakdown,
  isProxyAddress,
  startCommand,
  totalInputTokens,
} from "./format.ts";

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

test("call durations: ms under a second, one decimal under ten, then units", () => {
  const cases: Array<[number, string]> = [
    [0, "0 ms"],
    [812, "812 ms"],
    [999, "999 ms"],
    [1000, "1.0 s"],
    [3249, "3.2 s"],
    [9999, "9.9 s"],
    [10_000, "10 s"],
    [42_900, "42 s"],
    [60_000, "1 min"],
    [125_000, "2 min 5 s"],
    [2 * H + 5 * MIN, "2 h 5 min"],
    [-3, "0 ms"],
  ];
  for (const [ms, expected] of cases) {
    assert.equal(
      formatCallDuration(ms),
      expected.replace(/(\d) /g, "$1\u00a0"),
      `${ms} ms`,
    );
  }
});

test("call times: relative today, the date otherwise, the full time on hover", () => {
  const now = new Date(2026, 8, 27, 15, 30, 0).getTime();
  const today = (hour: number, minute: number, second = 0) =>
    new Date(2026, 8, 27, hour, minute, second).getTime();
  const text = (ms: number) => formatCallTime(ms, now, "en-US").text;
  assert.equal(text(now), "Just now");
  assert.equal(text(now - 59 * S), "Just now");
  assert.equal(text(now + 5 * S), "Just now"); // clocks that disagree
  // A no-break space between number and unit, as in every duration.
  assert.equal(text(now - 60 * S), "1 min ago");
  assert.equal(text(today(15, 18)), "12 min ago");
  assert.equal(text(today(12, 29)), "3 h ago");
  assert.equal(text(today(0, 0, 1)), "15 h ago");
  // Yesterday is a date, even when it was only minutes ago.
  const justAfterMidnight = new Date(2026, 8, 28, 0, 5).getTime();
  assert.equal(
    formatCallTime(
      new Date(2026, 8, 27, 23, 50).getTime(),
      justAfterMidnight,
      "en-US",
    ).text,
    "Sep 27",
  );
  assert.equal(text(new Date(2026, 8, 26, 14, 2).getTime()), "Sep 26");
  assert.equal(text(new Date(2025, 11, 31, 9).getTime()), "Dec 31, 2025");
  const { title } = formatCallTime(today(14, 2, 31), now, "en-US");
  assert.match(title, /^Sep 27, 2026, 2:02:31\sPM$/);
});

test("input tokens include the cache; the breakdown names each part", () => {
  const usage = {
    inputTokens: 408,
    outputTokens: 812,
    cacheCreationInputTokens: 0,
    cacheReadInputTokens: 12_000,
  };
  assert.equal(totalInputTokens(usage), 12_408);
  assert.equal(
    inputTokensBreakdown(usage, "en-US"),
    "12,408 input tokens: 408 uncached, 12,000 read from cache, 0 written to cache",
  );
  // A cache count the provider didn't report isn't shown as 0.
  const bare = {
    ...usage,
    cacheCreationInputTokens: null,
    cacheReadInputTokens: null,
  };
  assert.equal(totalInputTokens(bare), 408);
  assert.equal(
    inputTokensBreakdown(bare, "en-US"),
    "408 input tokens: 408 uncached",
  );
});

test("the ANTHROPIC_BASE_URL line, per platform", () => {
  assert.deepEqual(baseUrlCommands("127.0.0.1:4101", "posix"), [
    { shell: null, text: "export ANTHROPIC_BASE_URL=http://127.0.0.1:4101" },
  ]);
  assert.deepEqual(baseUrlCommands("127.0.0.1:4101", "windows"), [
    {
      shell: "PowerShell",
      text: '$env:ANTHROPIC_BASE_URL = "http://127.0.0.1:4101"',
    },
    // No quotes: cmd.exe's set would keep them as part of the value.
    {
      shell: "Command Prompt",
      text: "set ANTHROPIC_BASE_URL=http://127.0.0.1:4101",
    },
  ]);
});

test("only a 127.0.0.1:<port> address is put into a shell line", () => {
  for (const address of ["127.0.0.1:1", "127.0.0.1:65535"]) {
    assert.ok(isProxyAddress(address), address);
  }
  for (const address of [
    "127.0.0.1:0",
    "127.0.0.1:65536",
    "127.0.0.1:04101",
    "127.0.0.1",
    "localhost:4101",
    "0.0.0.0:4101",
    "127.0.0.1:4101 ",
    "127.0.0.1:4101;id",
    '127.0.0.1:4101"; Remove-Item ~ #',
    "127.0.0.1:4101$(id)",
  ]) {
    assert.equal(baseUrlCommands(address, "posix"), null, address);
    assert.equal(baseUrlCommands(address, "windows"), null, address);
  }
});

test("sh sets exactly the URL from the export line", {
  skip: process.platform === "win32",
}, () => {
  const [line] = baseUrlCommands("127.0.0.1:4101", "posix") ?? [];
  assert.ok(line);
  const value = execFileSync(
    "/bin/sh",
    ["-c", `${line.text}; printf %s "$ANTHROPIC_BASE_URL"`],
    { encoding: "utf8", env: {} },
  );
  assert.equal(value, "http://127.0.0.1:4101");
});
