import { commandPlatform, type DaemonStatus } from "../shared/daemon-status.ts";

const PLATFORM = commandPlatform(process.platform);

/** How often the daemon is checked while the window is visible. */
export const POLL_INTERVAL_MS = 3000;

/** The timer functions the poller uses; tests pass fakes. */
export interface Timers {
  setTimeout(callback: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface PollerOptions {
  check: () => Promise<DaemonStatus>;
  /** Called with every result, in order. */
  publish: (status: DaemonStatus) => void;
  intervalMs?: number;
  timers?: Timers;
}

const INITIAL_STATUS: DaemonStatus = {
  state: "connecting",
  message: "This usually takes less than a second.",
  checkedAtMs: 0,
  customDataDir: false,
  platform: PLATFORM,
};

/**
 * Checks the daemon every {@link POLL_INTERVAL_MS} while active, and not at all
 * while inactive (the window is hidden or minimised). The next check is
 * scheduled only after the previous one finished, so checks never overlap.
 * Becoming active checks straight away.
 */
export class StatusPoller {
  readonly #check: () => Promise<DaemonStatus>;
  readonly #publish: (status: DaemonStatus) => void;
  readonly #intervalMs: number;
  readonly #timers: Timers;
  #latest: DaemonStatus = INITIAL_STATUS;
  #active = false;
  #running = false;
  #again = false;
  #timer: unknown = null;

  constructor(options: PollerOptions) {
    this.#check = options.check;
    this.#publish = options.publish;
    this.#intervalMs = options.intervalMs ?? POLL_INTERVAL_MS;
    this.#timers = options.timers ?? {
      setTimeout: (callback, ms) => setTimeout(callback, ms),
      clearTimeout: (handle) =>
        clearTimeout(handle as ReturnType<typeof setTimeout>),
    };
  }

  /** The newest result, or "connecting" before the first check finishes. */
  get latest(): DaemonStatus {
    return this.#latest;
  }

  setActive(active: boolean): void {
    if (active === this.#active) {
      return;
    }
    this.#active = active;
    if (active) {
      void this.#tick();
    } else {
      this.#clearTimer();
    }
  }

  async #tick(): Promise<void> {
    this.#clearTimer();
    if (!this.#active) {
      return;
    }
    if (this.#running) {
      // Reactivated during a check: check again as soon as it finishes.
      this.#again = true;
      return;
    }
    this.#running = true;
    this.#again = false;
    try {
      this.#latest = await this.#check();
    } catch {
      // DaemonMonitor.check doesn't throw; this is the last resort.
      this.#latest = {
        state: "error",
        dataDir: null,
        reason: "unexpected",
        customDataDir: false,
        platform: PLATFORM,
        message: "Something unexpected went wrong while checking the daemon.",
        checkedAtMs: Date.now(),
      };
    } finally {
      this.#running = false;
    }
    try {
      this.#publish(this.#latest);
    } catch {
      // A window that is going away; the next check publishes again.
    }
    if (this.#active) {
      this.#timer = this.#timers.setTimeout(
        () => void this.#tick(),
        this.#again ? 0 : this.#intervalMs,
      );
    }
  }

  #clearTimer(): void {
    if (this.#timer !== null) {
      this.#timers.clearTimeout(this.#timer);
      this.#timer = null;
    }
  }
}
