import { APP_SCHEME } from "./renderer-files.ts";

/** The parts of Electron's Session that keeping the renderer on-machine uses. */
export interface OnMachineSession {
  setSpellCheckerEnabled(enable: boolean): void;
  webRequest: {
    onBeforeRequest(
      filter: { urls: string[] },
      listener: (
        details: { url: string },
        callback: (response: { cancel?: boolean }) => void,
      ) => void,
    ): void;
  };
}

/** Only the app's own `app://` pages may be loaded through the renderer session. */
export function isAllowedRequestUrl(url: string): boolean {
  try {
    return new URL(url).protocol === `${APP_SCHEME}:`;
  } catch {
    return false;
  }
}

/**
 * Enforces PRIVACY.md's "nothing leaves the machine" for the renderer session (#33):
 * - the built-in spellchecker is off for the whole session, since it downloads
 *   dictionaries from a Google CDN;
 * - every request that isn't to `app://` is cancelled, including ones the CSP doesn't
 *   cover. Main-process calls to the daemon must use Node's HTTP client, not this session.
 */
export function keepSessionOnMachine(session: OnMachineSession): void {
  session.setSpellCheckerEnabled(false);
  session.webRequest.onBeforeRequest(
    { urls: ["<all_urls>"] },
    (details, callback) =>
      callback({ cancel: !isAllowedRequestUrl(details.url) }),
  );
}
