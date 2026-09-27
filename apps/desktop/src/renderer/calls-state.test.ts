import assert from "node:assert/strict";
import { test } from "node:test";
import { NOT_RUNNING, page, positions, running } from "./call-fixtures.test.ts";
import {
  applyOlder,
  applyStatus,
  EMPTY_LIST,
  MAX_HELD_CALLS,
  UNREADABLE,
} from "./calls-state.ts";

const pos = (list: { calls: Array<{ globalPos: number }> }) =>
  list.calls.map((call) => call.globalPos);

test("the newest page is shown as it comes", () => {
  const list = applyStatus(EMPTY_LIST, running(page(positions(9, 7), 7)));
  assert.deepEqual(pos(list), [9, 7]);
  assert.equal(list.nextBefore, 7);
  assert.equal(list.loaded, true);
  assert.equal(list.problem, null);
});

test("Load older joins the older page below", () => {
  let list = applyStatus(EMPTY_LIST, running(page(positions(9, 7), 7)));
  const result = applyOlder(list, 7, page(positions(5, 3), 3));
  assert.equal(result.error, null);
  list = result.list;
  assert.deepEqual(pos(list), [9, 7, 5, 3]);
  assert.equal(list.nextBefore, 3);
  assert.equal(list.extended, true);
});

test("a refresh keeps the older calls when the new page reaches them", () => {
  let list = applyStatus(EMPTY_LIST, running(page(positions(9, 7), 7)));
  list = applyOlder(list, 7, page(positions(5, 3), 3)).list;
  // Two new calls: the page now ends at 9, which is held.
  list = applyStatus(list, running(page(positions(12, 11, 9), 9)));
  assert.deepEqual(pos(list), [12, 11, 9, 7, 5, 3]);
  assert.equal(list.nextBefore, 3);
});

test("a refresh with a gap before the held calls lets them go", () => {
  let list = applyStatus(EMPTY_LIST, running(page(positions(9, 7), 7)));
  list = applyOlder(list, 7, page(positions(5, 3), 3)).list;
  // More new calls than a page holds: 10 and 11 were never seen.
  list = applyStatus(list, running(page(positions(14, 13, 12), 12)));
  assert.deepEqual(pos(list), [14, 13, 12]);
  assert.equal(list.nextBefore, 12);
  assert.equal(list.extended, false);
});

test("a complete newest page replaces what is held", () => {
  let list = applyStatus(EMPTY_LIST, running(page(positions(9, 7), 7)));
  list = applyOlder(list, 7, page(positions(5), null)).list;
  list = applyStatus(list, running(page(positions(10, 9, 7, 5))));
  assert.deepEqual(pos(list), [10, 9, 7, 5]);
  assert.equal(list.nextBefore, null);
});

test("an empty older page with nextBefore moves the cursor on; more may exist", () => {
  // Unreadable records count towards a page, so a page can be empty and still
  // have a nextBefore (the oldest record scanned). Only its absence ends.
  let list = applyStatus(EMPTY_LIST, running(page(positions(90, 80), 60)));
  const empty = applyOlder(list, 60, page([], 40));
  assert.equal(empty.error, null);
  list = empty.list;
  assert.deepEqual(pos(list), [90, 80]);
  assert.equal(list.nextBefore, 40);
  list = applyOlder(list, 40, page(positions(30, 20))).list;
  assert.deepEqual(pos(list), [90, 80, 30, 20]);
  assert.equal(list.nextBefore, null);
});

test("an empty newest page with nextBefore isn't the end of the list", () => {
  let list = applyStatus(EMPTY_LIST, running(page([], 60)));
  assert.deepEqual(list.calls, []);
  assert.equal(list.nextBefore, 60);
  list = applyOlder(list, 60, page(positions(50, 40), 40)).list;
  assert.deepEqual(pos(list), [50, 40]);
  // The same empty newest page again: it covers 60 and up, which reaches what
  // is held, so the older calls stay.
  list = applyStatus(list, running(page([], 60)));
  assert.deepEqual(pos(list), [50, 40]);
  assert.equal(list.nextBefore, 40);
});

test("a nextBefore below a page's oldest call is the page's reach", () => {
  // Records 8 and 6 were scanned but unreadable: the page covers 6 and up.
  let list = applyStatus(EMPTY_LIST, running(page(positions(12, 10), 6)));
  list = applyOlder(list, 6, page(positions(5, 3), 3)).list;
  list = applyStatus(list, running(page(positions(14, 12, 10), 8)));
  assert.deepEqual(pos(list), [14, 12, 10, 5, 3]);
  assert.equal(list.nextBefore, 3);
});

test("a cursor that doesn't move back is refused, not followed", () => {
  const list = applyStatus(EMPTY_LIST, running(page(positions(9), 9)));
  const same = applyOlder(list, 9, page([], 9));
  assert.equal(same.list, list);
  assert.equal(same.error, "The daemon returned the same page again.");
});

test("an answer for a cursor the list moved past changes nothing", () => {
  const list = applyStatus(EMPTY_LIST, running(page(positions(9, 7), 7)));
  const result = applyOlder(list, 3, page(positions(2, 1)));
  assert.equal(result.list, list);
  assert.equal(result.error, null);
});

test("a failed Load older keeps the list and says why", () => {
  const list = applyStatus(EMPTY_LIST, running(page(positions(9, 7), 7)));
  const failed = applyOlder(list, 7, {
    state: "failed",
    message: "The daemon didn't return its recent calls.",
  });
  assert.equal(failed.list, list);
  assert.equal(failed.error, "The daemon didn't return its recent calls.");
  assert.equal(
    applyOlder(list, 7, { state: "loaded", calls: "?" }).error,
    UNREADABLE.state === "failed" ? UNREADABLE.message : "",
  );
});

test("a failed refresh keeps the calls shown and records the problem", () => {
  let list = applyStatus(EMPTY_LIST, running(page(positions(9, 7), 7)));
  list = applyStatus(
    list,
    running({
      state: "failed",
      message: "The daemon didn't return its recent calls.",
    }),
  );
  assert.deepEqual(pos(list), [9, 7]);
  assert.equal(list.problem?.state, "failed");
  // The next good page clears it.
  list = applyStatus(list, running(page(positions(9, 7), 7)));
  assert.equal(list.problem, null);
});

test("a payload that fails the checks is a problem, not calls", () => {
  const list = applyStatus(
    EMPTY_LIST,
    running({
      state: "loaded",
      calls: [{ globalPos: 1 }],
      nextBefore: null,
    } as never),
  );
  assert.deepEqual(list.calls, []);
  assert.deepEqual(list.problem, UNREADABLE);
});

test("another daemon run, or none, starts over", () => {
  let list = applyStatus(EMPTY_LIST, running(page(positions(9, 7), 7)));
  list = applyOlder(list, 7, page(positions(5, 3), 3)).list;
  const restarted = applyStatus(
    list,
    running(page(positions(2, 1)), { pid: 8 }),
  );
  assert.deepEqual(pos(restarted), [2, 1]);
  assert.equal(applyStatus(list, NOT_RUNNING), EMPTY_LIST);
});

test("at most MAX_HELD_CALLS are held; the cursor moves to the oldest kept", () => {
  const newest = Array.from({ length: 50 }, (_, i) => 5000 - i);
  let list = applyStatus(
    EMPTY_LIST,
    running(page(positions(...newest), newest.at(-1))),
  );
  while (list.nextBefore !== null && list.calls.length < MAX_HELD_CALLS + 50) {
    const before = list.nextBefore;
    const older = Array.from({ length: 50 }, (_, i) => before - 1 - i);
    const next = applyOlder(
      list,
      before,
      page(positions(...older), older.at(-1)),
    );
    if (next.list.calls.length === list.calls.length) {
      break;
    }
    list = next.list;
  }
  assert.equal(list.calls.length, MAX_HELD_CALLS);
  assert.equal(list.nextBefore, list.calls.at(-1)?.globalPos);
});
