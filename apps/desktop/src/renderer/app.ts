import type { CallsheetApi } from "../preload/api.ts";
import type { DaemonStatus } from "../shared/daemon-status.ts";
import {
  announce,
  focusRow,
  loadOlderHasFocus,
  loadOlderHidden,
  onLoadOlder,
  renderCalls,
  renderConnect,
} from "./calls-render.ts";
import { applyStatus, type CallsList, EMPTY_LIST } from "./calls-state.ts";
import {
  describeCalls,
  describeConnect,
  describeOlderLoaded,
} from "./calls-view.ts";
import { wireCopyButton } from "./copy.ts";
import {
  type DetailRow,
  describeStatus,
  type NextStep,
  type Notice,
} from "./describe.ts";
import {
  element,
  setBreakableText,
  setHidden,
  setOptionalText,
  setText,
} from "./dom.ts";
import { formatClock } from "./format.ts";
import { OlderLoader } from "./older-loader.ts";

/**
 * The status screen. Draws {@link describeStatus}'s view, the Connect Claude
 * Code card and the Recent calls list with plain DOM calls (`dom.ts`), changing
 * only what changed, so a three-second update neither re-announces the status
 * to screen readers nor clears a selection the user is making.
 */

declare global {
  interface Window {
    readonly callsheet: CallsheetApi;
  }
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
  daemon: element("daemon"),
  daemonTitle: element("daemon-title"),
  details: element<HTMLDListElement>("details"),
  checked: element("checked"),
};

/**
 * A warning is a callout above the details; information about them is a quiet
 * caption below, so it never outranks the facts it qualifies.
 */
function renderNotice(notice: Notice | null): void {
  const warning = notice?.tone === "warning" ? notice : null;
  const info = notice?.tone === "info" ? notice : null;
  setHidden(ui.notice, warning === null);
  if (warning) {
    setText(ui.noticeTitle, warning.title);
    setText(ui.noticeText, warning.text);
  }
  setHidden(ui.aside, info === null);
  if (info) {
    setText(ui.asideTitle, info.title);
    setText(ui.asideText, info.text);
  }
}

function renderNextStep(step: NextStep | null): void {
  setHidden(ui.nextStep, step === null);
  if (!step) {
    return;
  }
  setText(ui.nextStepText, step.text);
  setHidden(ui.commandRow, step.command === undefined);
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
  setHidden(ui.daemon, rows.length === 0);
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

/** The calls held, and "Load older". */
let calls: CallsList = EMPTY_LIST;
let latest: DaemonStatus | null = null;
const older = new OlderLoader({
  fetch: (before) => window.callsheet.calls.older(before),
  list: () => calls,
  settled: ({ list, loaded }) => {
    calls = list;
    const hadFocus = loadOlderHasFocus();
    renderCallsSection();
    if (loaded === null) {
      return; // a failure is the text by the button
    }
    announce(describeOlderLoaded(loaded.added, loaded.ended));
    // The button hid under the keyboard's focus: carry on at the first call
    // it loaded, rather than leaving focus on nothing.
    if (hadFocus && loadOlderHidden() && loaded.firstNew !== null) {
      focusRow(String(loaded.firstNew));
    }
  },
});

function renderCallsSection(): void {
  if (latest === null) {
    return;
  }
  renderConnect(describeConnect(latest));
  renderCalls(
    describeCalls(
      latest,
      calls,
      { loadingOlder: older.loading, olderError: older.error },
      Date.now(),
    ),
  );
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
  // Below the calls, the daemon's facts need a name of their own.
  setHidden(ui.daemonTitle, status.state !== "running");
  setText(
    ui.checked,
    status.checkedAtMs > 0
      ? `Last checked ${formatClock(status.checkedAtMs)} · Refreshes every 3 seconds`
      : "\u00a0",
  );
  latest = status;
  const before = calls.run;
  calls = applyStatus(calls, status);
  if (calls.run !== before) {
    older.reset();
  }
  renderCallsSection();
}

function loadOlder(): void {
  if (older.start()) {
    announce("");
    renderCallsSection();
  }
}

wireCopyButton(ui.copy, ui.command);
onLoadOlder(loadOlder);

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
  // The app's own main process refused or failed, which says nothing about the
  // daemon. A pushed status still replaces this if one arrives.
  initial({
    state: "error",
    dataDir: null,
    reason: "app",
    customDataDir: false,
    platform: navigator.userAgent.includes("Windows") ? "windows" : "posix",
    message:
      "This window couldn't get the daemon's status from the rest of Callsheet.",
    checkedAtMs: 0, // nothing was checked: no "Last checked" line
  });
});
