/**
 * The few DOM helpers the screen uses. Text is only ever set with
 * `textContent`, and only when it changes, so a three-second update neither
 * re-announces anything to screen readers nor clears a selection the user is
 * making.
 */

export function element<T extends HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (!found) {
    throw new Error(`#${id} is missing from index.html`);
  }
  return found as T;
}

export function setText(target: HTMLElement, text: string): void {
  if (target.textContent !== text) {
    target.textContent = text;
  }
}

/**
 * Like {@link setText}, for paths and commands: a line may break after each
 * path separator (a `<wbr>`, which copying leaves out) rather than mid-word.
 */
export function setBreakableText(target: HTMLElement, text: string): void {
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

export function setOptionalText(
  target: HTMLElement,
  text: string | null | undefined,
): void {
  const hidden = text === null || text === undefined;
  if (target.hidden !== hidden) {
    target.hidden = hidden;
  }
  setText(target, text ?? "");
}

/** Sets or removes an attribute, touching the DOM only on a change. */
export function setAttribute(
  target: HTMLElement,
  name: string,
  value: string | null,
): void {
  if (value === null) {
    if (target.hasAttribute(name)) {
      target.removeAttribute(name);
    }
  } else if (target.getAttribute(name) !== value) {
    target.setAttribute(name, value);
  }
}

export function setHidden(target: HTMLElement, hidden: boolean): void {
  if (target.hidden !== hidden) {
    target.hidden = hidden;
  }
}

export function create<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
): HTMLElementTagNameMap[K] {
  const created = document.createElement(tag);
  if (className) {
    created.className = className;
  }
  return created;
}
