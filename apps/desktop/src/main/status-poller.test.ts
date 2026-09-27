import assert from "node:assert/strict";
import { test } from "node:test";
import type { DaemonStatus } from "../shared/daemon-status.ts";
import { StatusPoller, type Timers } from "./status-poller.ts";

/** Timers that only fire when the test says so. */
class ManualTimers implements Timers {
  pending = new Map<number, { callback: () => void; ms: number }>();
  #next = 1;

  setTimeout(callback: () => void, ms: number): unknown {
    const id = this.#next++;
    this.pending.set(id, { callback, ms });
    return id;
  }

  clearTimeout(handle: unknown): void {
    this.pending.delete(handle as number);
  }

  fireAll(): void {
    const due = [...this.pending.values()];
    this.pending.clear();
    for (const { callback } of due) callback();
  }
}

function status(checkedAtMs: number): DaemonStatus {
  return {
    state: "not-running",
    dataDir: "/d",
    stale: false,
    customDataDir: false,
    message: "No daemon.",
    checkedAtMs,
  };
}

/** A check whose results the test releases one at a time. */
function controlledCheck() {
  const waiting: Array<(status: DaemonStatus) => void> = [];
  let calls = 0;
  return {
    check: () =>
      new Promise<DaemonStatus>((resolve) => {
        calls++;
        waiting.push(resolve);
      }),
    finish: async (value: DaemonStatus) => {
      const resolve = waiting.shift();
      assert.ok(resolve, "no check in flight");
      resolve(value);
      await new Promise((r) => setImmediate(r));
    },
    get calls() {
      return calls;
    },
    get inFlight() {
      return waiting.length;
    },
  };
}

test("starts as connecting and checks nothing while inactive", () => {
  const check = controlledCheck();
  const timers = new ManualTimers();
  const poller = new StatusPoller({
    check: check.check,
    publish: () => {},
    timers,
  });

  assert.equal(poller.latest.state, "connecting");
  assert.equal(poller.latest.checkedAtMs, 0);
  assert.equal(check.calls, 0);
  assert.equal(timers.pending.size, 0);
});

test("becoming active checks at once, then every interval", async () => {
  const check = controlledCheck();
  const timers = new ManualTimers();
  const published: DaemonStatus[] = [];
  const poller = new StatusPoller({
    check: check.check,
    publish: (s) => published.push(s),
    timers,
    intervalMs: 3000,
  });

  poller.setActive(true);
  assert.equal(check.calls, 1);
  await check.finish(status(1));
  assert.deepEqual(published, [status(1)]);
  assert.equal(poller.latest.checkedAtMs, 1);
  assert.deepEqual(
    [...timers.pending.values()].map((t) => t.ms),
    [3000],
  );

  timers.fireAll();
  assert.equal(check.calls, 2);
  await check.finish(status(2));
  assert.equal(published.length, 2);
});

test("checks never overlap: the next is scheduled only after one finishes", async () => {
  const check = controlledCheck();
  const timers = new ManualTimers();
  const poller = new StatusPoller({
    check: check.check,
    publish: () => {},
    timers,
  });

  poller.setActive(true);
  poller.setActive(false);
  poller.setActive(true); // while the first check is still in flight
  assert.equal(check.inFlight, 1);
  assert.equal(timers.pending.size, 0);

  await check.finish(status(1));
  assert.equal(check.calls, 1);
  // Reactivated mid-check, so the next check follows at once, not after 3 s.
  assert.deepEqual(
    [...timers.pending.values()].map((t) => t.ms),
    [0],
  );
});

test("a publish that throws doesn't stop polling", async () => {
  const check = controlledCheck();
  const timers = new ManualTimers();
  const poller = new StatusPoller({
    check: check.check,
    publish: () => {
      throw new Error("Render frame was disposed");
    },
    timers,
  });

  poller.setActive(true);
  await check.finish(status(1));
  assert.equal(poller.latest.checkedAtMs, 1);
  assert.equal(timers.pending.size, 1);
});

test("going inactive cancels the next check", async () => {
  const check = controlledCheck();
  const timers = new ManualTimers();
  const poller = new StatusPoller({
    check: check.check,
    publish: () => {},
    timers,
  });

  poller.setActive(true);
  await check.finish(status(1));
  poller.setActive(false);
  assert.equal(timers.pending.size, 0);
  assert.equal(check.calls, 1);
});

test("a check that finishes after going inactive publishes but doesn't reschedule", async () => {
  const check = controlledCheck();
  const timers = new ManualTimers();
  const published: DaemonStatus[] = [];
  const poller = new StatusPoller({
    check: check.check,
    publish: (s) => published.push(s),
    timers,
  });

  poller.setActive(true);
  poller.setActive(false);
  await check.finish(status(1));
  assert.equal(published.length, 1);
  assert.equal(timers.pending.size, 0);
});

test("a check that throws becomes an error status and polling continues", async () => {
  const timers = new ManualTimers();
  const published: DaemonStatus[] = [];
  const poller = new StatusPoller({
    check: () => Promise.reject(new Error("boom")),
    publish: (s) => published.push(s),
    timers,
  });

  poller.setActive(true);
  await new Promise((r) => setImmediate(r));
  assert.equal(published[0]?.state, "error");
  assert.ok(!JSON.stringify(published[0]).includes("boom"));
  assert.equal(timers.pending.size, 1);
});
