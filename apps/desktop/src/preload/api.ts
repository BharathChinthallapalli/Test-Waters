import type { DaemonStatus } from "../shared/daemon-status.ts";
import type { RecentCalls } from "../shared/recent-calls.ts";

/**
 * The API the preload script exposes to the renderer as `window.callsheet`.
 * Every member is added here first, with a type. The object and its members are
 * frozen; nothing here carries the control token or any other secret.
 */
export interface CallsheetApi {
  readonly status: StatusApi;
  readonly calls: CallsApi;
}

export interface StatusApi {
  /** The newest daemon status the main process has. */
  get(): Promise<DaemonStatus>;
  /**
   * Calls `listener` with every new status, about every 3 s while the window is
   * visible. Returns a function that stops the calls.
   */
  onChange(listener: (status: DaemonStatus) => void): () => void;
}

export interface CallsApi {
  /**
   * The page of recorded calls older than `before` (a page's `nextBefore`).
   * The newest page comes with every status instead.
   */
  older(before: number): Promise<RecentCalls>;
}
