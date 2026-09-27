import { applyOlder, type CallsList } from "./calls-state.ts";

/**
 * "Load older": one request at a time, and an answer applied only to the list
 * it was asked for. The list can move on while a request is out (a refresh
 * finds a gap and lets the older calls go, or a different daemon run starts),
 * and then the answer is dropped. No DOM, so the races are tested directly.
 */

/** What a successful load added, for the announcement and the focus. */
export interface OlderLoaded {
  added: number;
  /** The daemon said no older records remain. */
  ended: boolean;
  /** The position of the first call it added, if any. */
  firstNew: number | null;
}

export interface OlderSettled {
  list: CallsList;
  /** Said by the button when the load failed. */
  error: string | null;
  /** Null when it failed, or the answer was for a list that moved on. */
  loaded: OlderLoaded | null;
}

export interface OlderLoaderOptions {
  /** Asks the main process for the page before `before`. */
  fetch(before: number): Promise<unknown>;
  /** The list held now, read when a load starts and when its answer comes. */
  list(): CallsList;
  /** An answer arrived, or a load ended without one; redraw from this. */
  settled(result: OlderSettled): void;
}

/** What the renderer says when the main process itself didn't answer. */
export const OLDER_FAILED = "Older calls couldn't be loaded. Try again.";

export class OlderLoader {
  readonly #options: OlderLoaderOptions;
  #pending: { before: number; run: string | null } | null = null;
  #error: string | null = null;

  constructor(options: OlderLoaderOptions) {
    this.#options = options;
  }

  get loading(): boolean {
    return this.#pending !== null;
  }

  get error(): string | null {
    return this.#error;
  }

  /** Starts a load unless one is out or the list has ended. */
  start(): boolean {
    const list = this.#options.list();
    const before = list.nextBefore;
    if (before === null || this.#pending !== null) {
      return false;
    }
    const request = { before, run: list.run };
    this.#pending = request;
    this.#error = null;
    this.#options.fetch(before).then(
      (answer) => this.#settle(request, answer),
      () => this.#settle(request, { state: "failed", message: OLDER_FAILED }),
    );
    return true;
  }

  /** A different daemon run: forget the load in flight and any error. */
  reset(): void {
    this.#pending = null;
    this.#error = null;
  }

  #settle(request: { before: number; run: string | null }, answer: unknown) {
    if (this.#pending !== request) {
      return; // reset meanwhile: the list it was for is gone
    }
    this.#pending = null;
    const list = this.#options.list();
    if (list.run !== request.run || list.nextBefore !== request.before) {
      // The list moved on while the request was out.
      this.#options.settled({ list, error: null, loaded: null });
      return;
    }
    const held = list.calls.length;
    const result = applyOlder(list, request.before, answer);
    this.#error = result.error;
    if (result.error !== null) {
      this.#options.settled({
        list: result.list,
        error: result.error,
        loaded: null,
      });
      return;
    }
    this.#options.settled({
      list: result.list,
      error: null,
      loaded: {
        added: Math.max(0, result.list.calls.length - held),
        ended: result.list.nextBefore === null,
        firstNew: result.list.calls[held]?.globalPos ?? null,
      },
    });
  }
}
