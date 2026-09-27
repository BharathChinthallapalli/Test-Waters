/**
 * Copies the text of `source` when `button` is clicked. The Clipboard API needs
 * a permission this app denies to every page, so the text is selected and
 * copied with the editing command; if that fails the text stays selected for
 * the user to copy. The button says what happened for two seconds.
 */
export function wireCopyButton(button: HTMLButtonElement, source: HTMLElement) {
  let resetTimer: number | undefined;
  button.addEventListener("click", () => {
    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(source);
    selection?.removeAllRanges();
    selection?.addRange(range);
    let copied = false;
    try {
      copied = document.execCommand("copy");
    } catch {
      copied = false;
    }
    const shortcut = navigator.userAgent.includes("Mac") ? "⌘C" : "Ctrl+C";
    button.textContent = copied ? "Copied" : `Press ${shortcut}`;
    // A second click restarts the two seconds rather than racing the first timer.
    window.clearTimeout(resetTimer);
    resetTimer = window.setTimeout(() => {
      button.textContent = "Copy";
    }, 2000);
  });
}
