import type { CallsheetApi } from "../preload/api.ts";
import type { DaemonStatus } from "../shared/daemon-status.ts";
import {
  type DetailRow,
  describeStatus,
  type NextStep,
  type Notice,
} from "./describe.ts";
import { formatClock } from "./format.ts";

/**
 * The status screen. Draws {@link describeStatus}'s view with plain DOM calls.
 * Text is only ever set with `textContent`, and only when it changes, so a
 * three-second update neither re-announces the status to screen readers nor
 * clears a selection the user is making.
 */

declare global {
  interface Window {
    readonly callsheet: CallsheetApi;
  }
}

function element<T extends HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (!found) {
    throw new Error(`#${id} is missing from index.html`);
  }
  return found as T;
}

const ui = {
  mark: element("mark"),
  headline: element("headline"),
  message: element("message"),
  notice: element("notice"),
  aside: element("aside"),
  asideTitle: element("aside-title"),
  asideText: element("aside-text"),
  noticeTitle: element("notice-title"),
  noticeText: element("notice-text"),
  nextStep: element("next-step"),
  nextStepText: element("next-step-text"),
  commandRow: element("command-row"),
  command: element("command"),
  copy: element<HTMLButtonElement>("copy"),
  nextStepAfter: element("next-step-after"),
  details: element<HTMLDListElement>("details"),
  checked: element("checked"),
};

function setText(target: HTMLElement, text: string): void {
  if (target.textContent !== text) {
    target.textContent = text;
  }
}

/**
 * Like {@link setText}, for paths and commands: a line may break after each
 * path separator (a `<wbr>`, which copying leaves out) rather than mid-word.
 */
function setBreakableText(target: HTMLElement, text: string): void {
  if (target.textContent === text) {
    return;
  }
  const parts = text.split(/(?<=[/\\])/);
  target.replaceChildren(
    ...parts.flatMap((part, index) =>
      index === 0 ? [part] : [document.createElement("wbr"), part],
    ),
  );
}

function setOptionalText(target: HTMLElement, text: string | undefined): void {
  target.hidden = text === undefined;
  setText(target, text ?? "");
}

/**
 * A warning is a callout above the details; information about them is a quiet
 * caption below, so it never outranks the facts it qualifies.
 */
function renderNotice(notice: Notice | null): void {
  const warning = notice?.tone === "warning" ? notice : null;
  const info = notice?.tone === "info" ? notice : null;
  ui.notice.hidden = warning === null;
  if (warning) {
    setText(ui.noticeTitle, warning.title);
    setText(ui.noticeText, warning.text);
  }
  ui.aside.hidden = info === null;
  if (info) {
    setText(ui.asideTitle, info.title);
    setText(ui.asideText, info.text);
  }
}

function renderNextStep(step: NextStep | null): void {
  ui.nextStep.hidden = step === null;
  if (!step) {
    return;
  }
  setText(ui.nextStepText, step.text);
  ui.commandRow.hidden = step.command === undefined;
  setBreakableText(ui.command, step.command ?? "");
  setOptionalText(ui.nextStepAfter, step.after);
}

interface RowElements {
  row: HTMLDivElement;
  label: HTMLElement;
  value: HTMLSpanElement;
  note: HTMLSpanElement;
}

const rowElements = new Map<string, RowElements>();

function createRow(): RowElements {
  const row = document.createElement("div");
  row.className = "row";
  const label = document.createElement("dt");
  const detail = document.createElement("dd");
  const value = document.createElement("span");
  const note = document.createElement("span");
  note.className = "note";
  detail.append(value, note);
  row.append(label, detail);
  return { row, label, value, note };
}

/** Updates rows in place by key; adds, removes and reorders only as needed. */
function renderRows(rows: readonly DetailRow[]): void {
  ui.details.hidden = rows.length === 0;
  const keep = new Set(rows.map((row) => row.key));
  for (const [key, elements] of rowElements) {
    if (!keep.has(key)) {
      elements.row.remove();
      rowElements.delete(key);
    }
  }
  rows.forEach((row, index) => {
    let elements = rowElements.get(row.key);
    if (!elements) {
      elements = createRow();
      rowElements.set(row.key, elements);
    }
    setText(elements.label, row.label);
    if (row.mono) {
      setBreakableText(elements.value, row.value);
    } else {
      setText(elements.value, row.value);
    }
    const className = ["value", row.mono && "mono", row.muted && "muted"]
      .filter(Boolean)
      .join(" ");
    if (elements.value.className !== className) {
      elements.value.className = className;
    }
    setOptionalText(elements.note, row.note);
    if (ui.details.children[index] !== elements.row) {
      ui.details.insertBefore(elements.row, ui.details.children[index] ?? null);
    }
  });
}

function render(status: DaemonStatus): void {
  const view = describeStatus(status);
  if (ui.mark.dataset.tone !== view.tone) {
    ui.mark.dataset.tone = view.tone;
  }
  setText(ui.headline, view.headline);
  setText(ui.message, view.message);
  renderNotice(view.notice);
  renderNextStep(view.nextStep);
  renderRows(view.rows);
  setText(
    ui.checked,
    status.checkedAtMs > 0
      ? `Last checked ${formatClock(status.checkedAtMs)} · Refreshes every 3\u00a0seconds`
      : " ",
  );
}

/**
 * Copies the command. The Clipboard API needs a permission this app denies to
 * every page, so the command is selected and copied with the editing command;
 * if that fails the command stays selected for the user to copy.
 */
function copyCommand(): void {
  const selection = window.getSelection();
  const range = document.createRange();
  range.selectNodeContents(ui.command);
  selection?.removeAllRanges();
  selection?.addRange(range);
  let copied = false;
  try {
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  }
  const shortcut = navigator.userAgent.includes("Mac") ? "⌘C" : "Ctrl+C";
  ui.copy.textContent = copied ? "Copied" : `Press ${shortcut}`;
  window.setTimeout(() => {
    ui.copy.textContent = "Copy";
  }, 2000);
}

ui.copy.addEventListener("click", copyCommand);

// Pushed statuses are always the newest. The answer to the first `get` is only
// shown if no push arrived before it (ordering by arrival, not by the clock).
let pushed = false;
window.callsheet.status.onChange((status) => {
  pushed = true;
  render(status);
});
const initial = (status: DaemonStatus): void => {
  if (!pushed) {
    render(status);
  }
};
window.callsheet.status.get().then(initial, () => {
  // The main process refused or failed; the next pushed status replaces this.
  initial({
    state: "error",
    dataDir: null,
    reason: "unexpected",
    customDataDir: false,
    message: "The app couldn't ask its main process for the daemon's status.",
    checkedAtMs: Date.now(),
  });
});
