import type { DaemonStatus } from "../shared/daemon-status.ts";
import type { CallSummary, RecentCalls } from "../shared/recent-calls.ts";
import { parseRecentCalls } from "./calls-payload.ts";

/**
 * The calls the screen holds: the newest page, which comes with every status,
 * joined to the older pages "Load older" fetched. Pure, so the joining rules
 * are tested without a DOM.
 *
 * Paging (`calls.list`): a page's `nextBefore` is the position of the oldest
 * record the daemon scanned for it, not of the oldest call it returned; a
 * record it couldn't read still counts towards the page size. So a page covers
 * every position from its `nextBefore` up, a page may be empty and still have a
 * `nextBefore`, and only a page without `nextBefore` ends the list.
 *
 * The newest page joins the calls already held whenever its range reaches the
 * newest of them, so calls below it stay as new ones arrive; if more records
 * arrived between two checks than a page scans, there would be a gap, so the
 * held calls below it are let go instead. A different daemon run starts over.
 */

/** Calls held at most; older ones can be fetched again with "Load older". */
export const MAX_HELD_CALLS = 1000;

export interface CallsList {
  /** The daemon run the calls came from: address and pid. */
  run: string | null;
  /** Newest first, strictly falling positions. */
  calls: CallSummary[];
  /** Cursor for the next older page; null only once the list has ended. */
  nextBefore: number | null;
  /**
   * The last newest page's `nextBefore`: that page covered every position from
   * here up, whether or not it returned calls.
   */
  newestFrom: number | null;
  /** At least one page arrived for this run. */
  loaded: boolean;
  /** The newest page's last answer, when it wasn't a page. */
  problem: Exclude<RecentCalls, { state: "loaded" }> | null;
}

export const EMPTY_LIST: CallsList = Object.freeze({
  run: null,
  calls: [],
  nextBefore: null,
  newestFrom: null,
  loaded: false,
  problem: null,
}) as CallsList;

/** What the screen says when the IPC payload fails the checks. */
export const UNREADABLE: RecentCalls = {
  state: "failed",
  message: "The app received a list of calls it couldn't read.",
};

/** Keeps at most {@link MAX_HELD_CALLS}, moving the cursor to the oldest kept. */
function capped(
  calls: CallSummary[],
  nextBefore: number | null,
): Pick<CallsList, "calls" | "nextBefore"> {
  if (calls.length <= MAX_HELD_CALLS) {
    return { calls, nextBefore };
  }
  const kept = calls.slice(0, MAX_HELD_CALLS);
  return { calls: kept, nextBefore: kept.at(-1)?.globalPos ?? null };
}

/** The list after a new status: the newest page joined to what is held. */
export function applyStatus(list: CallsList, status: DaemonStatus): CallsList {
  if (status.state !== "running") {
    return EMPTY_LIST;
  }
  const run = `${status.address}|${status.pid}`;
  const current = list.run === run ? list : { ...EMPTY_LIST, run };
  const page = parseRecentCalls(status.calls) ?? UNREADABLE;
  if (page.state !== "loaded") {
    // Keep what is shown; the screen says it couldn't be refreshed.
    return { ...current, problem: page };
  }
  // The page covers every position from `from` up. What is held covers one
  // contiguous range up to the newest held call and, since the last newest
  // page, up from that page's `from` too. When the two overlap, the held calls
  // below the page stay, whether they came from "Load older" or from an
  // earlier newest page, so rows don't fall off the bottom as calls arrive.
  const from = page.nextBefore;
  const newestHeld = current.calls[0]?.globalPos ?? null;
  const reaches = (top: number | null) =>
    from !== null && top !== null && from <= top;
  if (from === null || !(reaches(newestHeld) || reaches(current.newestFrom))) {
    return {
      run,
      calls: page.calls,
      nextBefore: page.nextBefore,
      newestFrom: page.nextBefore,
      loaded: true,
      problem: null,
    };
  }
  const older = current.calls.filter((call) => call.globalPos < from);
  const nextBefore =
    current.nextBefore === null ? null : Math.min(current.nextBefore, from);
  return {
    run,
    ...capped([...page.calls, ...older], nextBefore),
    newestFrom: from,
    loaded: true,
    problem: null,
  };
}

/**
 * The list after "Load older" answered for `before`. An answer for a cursor the
 * list no longer has (the list moved on meanwhile) changes nothing. An empty
 * page with a `nextBefore` moves the cursor on and keeps "Load older". Returns
 * the list and, when loading failed, the sentence to show by the button.
 */
export function applyOlder(
  list: CallsList,
  before: number,
  answer: unknown,
): { list: CallsList; error: string | null } {
  if (list.nextBefore !== before) {
    return { list, error: null };
  }
  const page = parseRecentCalls(answer) ?? UNREADABLE;
  if (page.state === "failed") {
    return { list, error: page.message };
  }
  if (page.state === "unsupported") {
    return { list, error: "This daemon can't list older calls." };
  }
  if (page.nextBefore !== null && page.nextBefore >= before) {
    // A cursor that doesn't move back would ask for the same page forever.
    return { list, error: "The daemon returned the same page again." };
  }
  const older = page.calls.filter((call) => call.globalPos < before);
  return {
    list: {
      ...list,
      ...capped([...list.calls, ...older], page.nextBefore),
    },
    error: null,
  };
}
