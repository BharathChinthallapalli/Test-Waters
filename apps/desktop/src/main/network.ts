import { APP_SCHEME } from "./renderer-files.ts";

/** The parts of Electron's Session that keeping the renderer on-machine uses. */
export interface OnMachineSession {
  setSpellCheckerEnabled(enable: boolean): void;
  setSpellCheckerLanguages(languages: string[]): void;
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
 * - the built-in spellchecker is off for the whole session, with no languages set.
 *   Turning it off alone is not enough: on Windows and Linux the session starts
 *   downloading the system language's Hunspell dictionary from a Google CDN
 *   (`redirector.gvt1.com/edgedl/chrome/dict/…`) through a loader `webRequest` does
 *   not see. Clearing the languages stops that download (checked with a logging
 *   proxy: one request per start before, none after);
 * - every request that isn't to `app://` is cancelled, including ones the CSP doesn't
 *   cover. Main-process calls to the daemon must use Node's HTTP client, not this session.
 */
export function keepSessionOnMachine(session: OnMachineSession): void {
  session.setSpellCheckerLanguages([]);
  session.setSpellCheckerEnabled(false);
  session.webRequest.onBeforeRequest(
    { urls: ["<all_urls>"] },
    (details, callback) =>
      callback({ cancel: !isAllowedRequestUrl(details.url) }),
  );
}
