import type {
  CallDetail,
  CallRowView,
  CallsView,
  ConnectView,
} from "./calls-view.ts";
import { wireCopyButton } from "./copy.ts";
import {
  create,
  element,
  setAttribute,
  setBreakableText,
  setHidden,
  setOptionalText,
  setText,
} from "./dom.ts";

/**
 * Draws the "Connect Claude Code" card and the "Recent calls" list from their
 * views (`calls-view.ts`). Rows are kept by the call's position and updated in
 * place, so a refresh keeps focus, open details and any text selection.
 *
 * Each row is a disclosure (WAI-ARIA APG, "Disclosure (Show/Hide) Pattern"): a
 * native button with `aria-expanded` and `aria-controls`, so Tab reaches it and
 * Enter or Space opens and closes its details.
 */

const ui = {
  connect: element("connect"),
  connectTitle: element("connect-title"),
  connectText: element("connect-text"),
  connectCommands: element("connect-commands"),
  connectAfter: element("connect-after"),
  calls: element("calls"),
  dropped: element("calls-dropped"),
  note: element("calls-note"),
  state: element("calls-state"),
  message: element("calls-message"),
  detail: element("calls-detail"),
  columns: element("calls-columns"),
  list: element<HTMLUListElement>("call-list"),
  more: element("calls-more"),
  loadOlder: element<HTMLButtonElement>("load-older"),
  olderError: element("older-error"),
  limit: element("calls-limit"),
  announce: element("calls-announce"),
};

/** The commands shown now, so the card is rebuilt only when they change. */
let shownCommands = "";

export function renderConnect(view: ConnectView | null): void {
  setHidden(ui.connect, view === null);
  if (view === null) {
    return;
  }
  if (view.kind === "unavailable") {
    setText(ui.connectTitle, view.title);
    setText(ui.connectText, view.text);
    setOptionalText(ui.connectAfter, null);
    setHidden(ui.connectCommands, true);
    return;
  }
  setText(ui.connectTitle, "Connect Claude Code");
  setText(ui.connectText, view.text);
  setOptionalText(ui.connectAfter, view.after);
  setHidden(ui.connectCommands, false);
  const key = JSON.stringify(view.commands);
  if (key === shownCommands) {
    return;
  }
  shownCommands = key;
  ui.connectCommands.replaceChildren(
    ...view.commands.map((command) => {
      const block = create("div", "connect-command");
      if (command.shell !== null) {
        const shell = create("p", "connect-shell");
        shell.textContent = command.shell;
        block.append(shell);
      }
      const row = create("div", "command-row");
      const code = create("code", "command");
      setBreakableText(code, command.text);
      const button = create("button", "button");
      button.type = "button";
      button.textContent = "Copy";
      // "Copy" alone is ambiguous when there are two commands.
      if (command.shell !== null) {
        button.setAttribute("aria-label", `Copy the ${command.shell} command`);
      }
      wireCopyButton(button, code);
      row.append(code, button);
      block.append(row);
      return block;
    }),
  );
}

interface RowElements {
  item: HTMLLIElement;
  button: HTMLButtonElement;
  time: HTMLSpanElement;
  model: HTMLSpanElement;
  path: HTMLSpanElement;
  stream: HTMLSpanElement;
  status: HTMLSpanElement;
  statusText: HTMLSpanElement;
  tokensIn: HTMLSpanElement;
  tokensInValue: HTMLSpanElement;
  tokensOut: HTMLSpanElement;
  tokensOutValue: HTMLSpanElement;
  duration: HTMLSpanElement;
  details: HTMLDivElement;
  /** The details are built on first open, and kept up to date after that. */
  built: boolean;
}

const rows = new Map<string, RowElements>();
/** Rows whose details are open, by key; kept across refreshes. */
const expanded = new Set<string>();
let latestViews = new Map<string, CallRowView>();

function span(className: string, text?: string): HTMLSpanElement {
  const created = create("span", className);
  if (text !== undefined) {
    created.textContent = text;
  }
  return created;
}

function createRow(key: string): RowElements {
  const item = create("li", "call");
  const button = create("button", "call-row");
  button.type = "button";
  const detailsId = `call-details-${key}`;
  button.setAttribute("aria-controls", detailsId);
  button.setAttribute("aria-expanded", "false");

  const time = span("call-time");
  const modelCell = span("call-model");
  const model = span("call-model-name");
  const path = span("call-path");
  const stream = span("call-stream", "stream");
  modelCell.append(model, path, stream);
  const status = span("call-status");
  const statusText = span("call-status-text");
  status.append(span("call-mark"), statusText);
  const tokensIn = span("call-num call-in");
  const tokensInValue = span("");
  tokensIn.append(tokensInValue, span("call-unit", " in"));
  const tokensOut = span("call-num call-out");
  const tokensOutValue = span("");
  tokensOut.append(tokensOutValue, span("call-unit", " out"));
  const duration = span("call-num call-duration");
  button.append(
    span("call-chevron"),
    time,
    modelCell,
    status,
    tokensIn,
    tokensOut,
    duration,
  );

  const details = create("div", "call-details");
  details.id = detailsId;
  details.hidden = true;
  item.append(button, details);

  const elements: RowElements = {
    item,
    button,
    time,
    model,
    path,
    stream,
    status,
    statusText,
    tokensIn,
    tokensInValue,
    tokensOut,
    tokensOutValue,
    duration,
    details,
    built: false,
  };
  button.addEventListener("click", () => toggle(key, elements));
  return elements;
}

function toggle(key: string, elements: RowElements): void {
  const open = !expanded.has(key);
  if (open) {
    expanded.add(key);
  } else {
    expanded.delete(key);
  }
  const view = latestViews.get(key);
  if (open && view) {
    renderDetails(elements, view);
  }
  setAttribute(elements.button, "aria-expanded", String(open));
  setHidden(elements.details, !open);
}

function detailRow(detail: CallDetail): HTMLDivElement {
  const row = create("div", "call-fact");
  row.dataset.key = detail.key;
  const label = create("dt");
  label.textContent = detail.label;
  const value = create("dd", detail.mono ? "mono" : undefined);
  setBreakableText(value, detail.value);
  row.append(label, value);
  return row;
}

/** Builds or updates a row's details. Built once; later only text changes. */
function renderDetails(elements: RowElements, view: CallRowView): void {
  const key = JSON.stringify([view.details, view.headers]);
  if (elements.details.dataset.content === key) {
    return;
  }
  elements.details.dataset.content = key;
  const facts = create("dl", "call-facts");
  facts.append(...view.details.map(detailRow));
  const parts: HTMLElement[] = [facts];
  if (view.headers.length > 0) {
    const section = create("div", "call-headers");
    const title = create("h3", "call-headers-title");
    title.textContent = "Rate-limit headers";
    const list = create("dl", "call-header-list");
    for (const [name, value] of view.headers) {
      const entry = create("div", "call-header");
      const term = create("dt");
      term.textContent = name;
      const description = create("dd");
      description.textContent = value;
      entry.append(term, description);
      list.append(entry);
    }
    section.append(title, list);
    parts.push(section);
  }
  // A record doesn't change once written, so this runs once per row in practice.
  elements.details.replaceChildren(...parts);
  elements.built = true;
}

function updateRow(elements: RowElements, view: CallRowView): void {
  setAttribute(elements.button, "aria-label", view.label);
  setText(elements.time, view.time);
  setAttribute(elements.time, "title", view.timeTitle);
  // No model: the path stands alone, or a dash says nothing was reported.
  setText(elements.model, view.model ?? "—");
  elements.model.classList.toggle("muted", view.model === null);
  setHidden(elements.model, view.model === null && view.path !== null);
  setOptionalText(elements.path, view.path);
  setHidden(elements.stream, !view.streamed);
  if (elements.status.dataset.tone !== view.outcome.tone) {
    elements.status.dataset.tone = view.outcome.tone;
  }
  setText(elements.statusText, view.outcome.label);
  setText(elements.tokensInValue, view.tokensIn);
  setAttribute(elements.tokensIn, "title", view.tokensInTitle);
  setText(elements.tokensOutValue, view.tokensOut);
  setText(elements.duration, view.duration);
  const noUsage = view.tokensInTitle === null;
  elements.tokensIn.classList.toggle("muted", noUsage);
  elements.tokensOut.classList.toggle("muted", noUsage);
  const open = expanded.has(view.key);
  setAttribute(elements.button, "aria-expanded", String(open));
  setHidden(elements.details, !open);
  if (open || elements.built) {
    renderDetails(elements, view);
  }
}

/**
 * Updates rows in place by key. New calls arrive at the top, so existing rows
 * never move and a focused row keeps its focus.
 */
function renderRows(views: readonly CallRowView[]): void {
  latestViews = new Map(views.map((view) => [view.key, view]));
  for (const [key, elements] of rows) {
    if (!latestViews.has(key)) {
      elements.item.remove();
      rows.delete(key);
      expanded.delete(key);
    }
  }
  views.forEach((view, index) => {
    let elements = rows.get(view.key);
    if (!elements) {
      elements = createRow(view.key);
      rows.set(view.key, elements);
    }
    updateRow(elements, view);
    if (ui.list.children[index] !== elements.item) {
      ui.list.insertBefore(elements.item, ui.list.children[index] ?? null);
    }
  });
}

export function renderCalls(view: CallsView | null): void {
  setHidden(ui.calls, view === null);
  if (view === null) {
    renderRows([]);
    return;
  }
  setOptionalText(ui.dropped, view.dropped);
  setOptionalText(ui.note, view.note);
  const listed = view.state === "list";
  setHidden(ui.state, listed);
  if (ui.state.dataset.state !== view.state) {
    ui.state.dataset.state = view.state;
  }
  setText(ui.message, view.message ?? "");
  setOptionalText(ui.detail, view.detail);
  setHidden(ui.columns, !listed);
  setHidden(ui.list, !listed);
  renderRows(view.rows);

  setHidden(ui.more, view.loadOlder === "hidden");
  const loading = view.loadOlder === "loading";
  setText(ui.loadOlder, loading ? "Loading…" : "Load older");
  // Not `disabled`: a disabled button drops focus; aria-disabled keeps it.
  setAttribute(ui.loadOlder, "aria-disabled", loading ? "true" : null);
  setText(ui.olderError, view.olderError ?? "");
  setOptionalText(ui.limit, view.limitNote);
}

/**
 * Says `text` politely through the section's status region. Cleared first, so
 * the same sentence twice in a row is still announced.
 */
export function announce(text: string): void {
  ui.announce.textContent = "";
  if (text !== "") {
    // After the clearing is seen, or the same text isn't a change.
    requestAnimationFrame(() => {
      ui.announce.textContent = text;
    });
  }
}

/** True while keyboard or pointer focus is on "Load older". */
export function loadOlderHasFocus(): boolean {
  return document.activeElement === ui.loadOlder;
}

/**
 * True when "Load older" is hidden. Asked straight after a render: Chromium
 * moves focus off a hidden element only at its next style update, so
 * {@link loadOlderHasFocus} can still be true then.
 */
export function loadOlderHidden(): boolean {
  return ui.more.hidden === true;
}

/** Moves focus to the row of the call at this position, if it is shown. */
export function focusRow(key: string): void {
  rows.get(key)?.button.focus();
}

/** Calls `load` when "Load older" is pressed, unless a load is running. */
export function onLoadOlder(load: () => void): void {
  ui.loadOlder.addEventListener("click", () => {
    if (ui.loadOlder.getAttribute("aria-disabled") !== "true") {
      load();
    }
  });
}
