import { APP_SCHEME, RENDERER_HOST } from "./renderer-files.ts";

/**
 * IPC channel names. The sandboxed preload can't import modules, so
 * `src/preload/index.cts` repeats these strings; a test keeps them identical.
 */
export const STATUS_GET_CHANNEL = "callsheet:status:get";
export const STATUS_CHANGED_CHANNEL = "callsheet:status:changed";

/** The renderer's origin: the only one allowed to use the preload API. */
export const RENDERER_ORIGIN = `${APP_SCHEME}://${RENDERER_HOST}`;

/** The parts of an `IpcMainInvokeEvent` that sender validation reads. */
export interface IpcSender {
  sender: unknown;
  senderFrame: { origin: string; parent: unknown } | null;
}

/**
 * True only for the top frame of the app's own window, showing an
 * `app://renderer` page. Per Electron's security guide (item 17, "Validate the
 * sender of all IPC messages") the frame's origin is checked, not its URL, and a
 * frame that has gone away (null) is refused.
 */
export function isTrustedSender(event: IpcSender, trusted: unknown): boolean {
  const frame = event.senderFrame;
  return (
    trusted !== null &&
    event.sender === trusted &&
    frame !== null &&
    frame.parent === null &&
    frame.origin === RENDERER_ORIGIN
  );
}
