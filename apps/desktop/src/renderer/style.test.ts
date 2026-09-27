import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

/**
 * Checks the checkable parts of `.kiro/steering/ui.md` against style.css:
 * WCAG 2.2 contrast of the colour tokens in both themes, and the rules every
 * screen must have.
 */

const css = await readFile(new URL("./style.css", import.meta.url), "utf8");

function tokens(block: string): Map<string, string> {
  const found = new Map<string, string>();
  for (const [, name, value] of block.matchAll(
    /(--color-[\w-]+):\s*(#[0-9a-f]{6});/gi,
  )) {
    found.set(name as string, (value as string).toLowerCase());
  }
  return found;
}

const lightBlock = /^:root \{([\s\S]*?)^\}/m.exec(css)?.[1] ?? "";
const darkBlock =
  /@media \(prefers-color-scheme: dark\) \{\s*:root \{([\s\S]*?)\}/.exec(
    css,
  )?.[1] ?? "";
const light = tokens(lightBlock);
const dark = tokens(darkBlock);

/** Relative luminance, WCAG 2.2 "relative luminance" definition. */
function luminance(hex: string): number {
  const channels = [1, 3, 5].map(
    (i) => Number.parseInt(hex.slice(i, i + 2), 16) / 255,
  );
  const [r, g, b] = channels.map((c) =>
    c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4,
  ) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function colour(theme: Map<string, string>, token: string): string {
  const value = theme.get(`--color-${token}`);
  assert.ok(value, `--color-${token} is defined`);
  return value;
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [
    number,
    number,
  ];
  return (hi + 0.05) / (lo + 0.05);
}

/** Text: 4.5:1 (SC 1.4.3). */
const TEXT_PAIRS: Array<[string, string]> = [
  ["text", "bg"],
  ["text", "surface"],
  ["text", "code-bg"],
  ["text", "button"],
  ["text", "button-hover"],
  ["text", "info-subtle"],
  ["text", "warning-subtle"],
  ["text-secondary", "bg"],
  ["text-secondary", "surface"],
  ["text-secondary", "info-subtle"],
  ["text-secondary", "warning-subtle"],
  ["text-tertiary", "bg"],
  ["text-tertiary", "surface"],
  // Recent calls: rows on the card, and open or hovered rows.
  ["text", "row-active"],
  ["text-secondary", "row-active"],
  ["text-tertiary", "row-active"],
  ["warning", "surface"],
  ["warning", "row-active"],
  ["danger", "surface"],
  ["danger", "row-active"],
];

/** Icons, status marks and the focus ring: 3:1 (SC 1.4.11). */
const UI_PAIRS: Array<[string, string]> = [
  ...["ok", "off", "pending", "warning", "danger", "info"].map(
    (tone): [string, string] => [tone, `${tone}-subtle`],
  ),
  ...["ok", "warning", "danger", "info"].map((tone): [string, string] => [
    "on-mark",
    tone,
  ]),
  ["focus", "bg"],
  ["focus", "surface"],
  ["focus", "row-active"],
  // A call's outcome mark, on a row and on an open row.
  ...["ok", "off", "warning", "danger"].flatMap((tone): [string, string][] => [
    [tone, "surface"],
    [tone, "row-active"],
  ]),
];

for (const [theme, colours] of [
  ["light", light],
  ["dark", dark],
] as const) {
  test(`${theme} theme: text contrast is at least 4.5:1`, () => {
    for (const [fg, bg] of TEXT_PAIRS) {
      const ratio = contrast(colour(colours, fg), colour(colours, bg));
      assert.ok(ratio >= 4.5, `${fg} on ${bg}: ${ratio.toFixed(2)}`);
    }
  });

  test(`${theme} theme: marks, icons and focus are at least 3:1`, () => {
    for (const [fg, bg] of UI_PAIRS) {
      const ratio = contrast(colour(colours, fg), colour(colours, bg));
      assert.ok(ratio >= 3, `${fg} on ${bg}: ${ratio.toFixed(2)}`);
    }
  });
}

test("the dark theme redefines every colour token", () => {
  assert.ok(light.size > 20, "light tokens parsed");
  assert.deepEqual([...dark.keys()].sort(), [...light.keys()].sort());
});

test("colours are used only through tokens", () => {
  const outsideTokens = css.replace(lightBlock, "").replace(darkBlock, "");
  assert.doesNotMatch(outsideTokens, /#[0-9a-f]{3,8}\b|rgba?\(|hsla?\(/i);
});

test("motion stops for prefers-reduced-motion", () => {
  assert.match(
    css,
    /@media \(prefers-reduced-motion: reduce\) \{\s*\.mark-arc \{\s*animation: none;/,
  );
});

test("keyboard focus is always visible", () => {
  assert.match(
    css,
    /:focus-visible \{\s*outline: 2px solid var\(--color-focus\);/,
  );
});

test("numbers use tabular figures", () => {
  assert.match(css, /font-variant-numeric: tabular-nums;/);
});

test("no decoration the standards rule out, and nothing loaded from elsewhere", () => {
  for (const banned of [
    /gradient\(/,
    /backdrop-filter/,
    /@import/,
    /@font-face/,
    /url\(/,
    /text-shadow/,
  ]) {
    assert.doesNotMatch(css, banned);
  }
});
