/** The parts of Electron's WebContents that navigation hardening uses. */
export interface NavigableContents {
  on(
    event: "will-navigate",
    listener: (event: { preventDefault(): void }) => void,
  ): unknown;
  setWindowOpenHandler(handler: () => { action: "deny" }): void;
}

/**
 * The renderer shows one page and never needs to leave it, so every
 * navigation and every new-window request is refused.
 */
export function blockNavigation(contents: NavigableContents): void {
  contents.on("will-navigate", (event) => {
    event.preventDefault();
  });
  contents.setWindowOpenHandler(() => ({ action: "deny" }));
}
