import assert from "node:assert/strict";
import { test } from "node:test";
import { page, positions, running } from "./call-fixtures.test.ts";
import { applyStatus, type CallsList, EMPTY_LIST } from "./calls-state.ts";
import {
  OLDER_FAILED,
  OlderLoader,
  type OlderSettled,
} from "./older-loader.ts";

/** A loader whose requests are answered by hand, in any order. */
function harness(initial: CallsList) {
  let list = initial;
  const requests: Array<{
    before: number;
    answer: (value: unknown) => Promise<void>;
    fail: () => Promise<void>;
  }> = [];
  const settled: OlderSettled[] = [];
  const loader = new OlderLoader({
    fetch: (before) =>
      new Promise((resolve, reject) => {
        const flush = () => new Promise<void>((done) => setImmediate(done));
        requests.push({
          before,
          answer: (value) => {
            resolve(value);
            return flush();
          },
          fail: () => {
            reject(new Error("ipc"));
            return flush();
          },
        });
      }),
    list: () => list,
    settled: (result) => {
      list = result.list;
      settled.push(result);
    },
  });
  return {
    loader,
    requests,
    settled,
    list: () => list,
    setList: (next: CallsList) => {
      list = next;
    },
  };
}

const withCursor = () =>
  applyStatus(EMPTY_LIST, running(page(positions(90, 80), 60)));

test("one request at a time; the answer is applied and described", async () => {
  const h = harness(withCursor());
  assert.equal(h.loader.start(), true);
  assert.equal(h.loader.loading, true);
  assert.equal(h.loader.start(), false, "a second press while loading");
  assert.equal(h.requests.length, 1);
  await h.requests[0]?.answer(page(positions(50, 40)));
  assert.equal(h.loader.loading, false);
  assert.deepEqual(h.settled.at(-1)?.loaded, {
    added: 2,
    ended: true,
    firstNew: 50,
  });
  assert.equal(h.loader.start(), false, "the list has ended");
});

test("an empty page with a cursor adds nothing and doesn't end the list", async () => {
  const h = harness(withCursor());
  h.loader.start();
  await h.requests[0]?.answer(page([], 40));
  assert.deepEqual(h.settled.at(-1)?.loaded, {
    added: 0,
    ended: false,
    firstNew: null,
  });
  assert.equal(h.loader.start(), true);
  assert.equal(h.requests[1]?.before, 40);
});

test("an answer for a list that moved on is dropped, not announced", async () => {
  const h = harness(withCursor());
  h.loader.start();
  // A refresh found a gap and replaced the list meanwhile: a new cursor.
  h.setList(applyStatus(h.list(), running(page(positions(200, 190), 180))));
  await h.requests[0]?.answer(page(positions(50, 40)));
  const last = h.settled.at(-1);
  assert.equal(last?.loaded, null);
  assert.equal(last?.error, null);
  assert.deepEqual(
    h.list().calls.map((call) => call.globalPos),
    [200, 190],
  );
  assert.equal(h.loader.loading, false);
});

test("after a reset (another daemon run) a late answer does nothing", async () => {
  const h = harness(withCursor());
  h.loader.start();
  h.loader.reset();
  assert.equal(h.loader.loading, false);
  await h.requests[0]?.answer(page(positions(50, 40)));
  assert.equal(h.settled.length, 0);
  // And a new load can start at once.
  assert.equal(h.loader.start(), true);
});

test("a failure is kept for the button until the next load starts", async () => {
  const h = harness(withCursor());
  h.loader.start();
  await h.requests[0]?.fail();
  assert.equal(h.loader.error, OLDER_FAILED);
  assert.equal(h.settled.at(-1)?.loaded, null);
  h.loader.start();
  assert.equal(h.loader.error, null);
  await h.requests[1]?.answer({ state: "failed", message: "No." });
  assert.equal(h.loader.error, "No.");
});
