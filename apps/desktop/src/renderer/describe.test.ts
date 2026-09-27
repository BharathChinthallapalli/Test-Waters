import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  DaemonHealth,
  DaemonStatus,
  ErrorReason,
  RunningStatus,
} from "../shared/daemon-status.ts";
import { describeStatus, type StatusView } from "./describe.ts";

const base = {
  message: "What happened.",
  checkedAtMs: 1,
  customDataDir: false,
};
const dataDir = "/home/ann/.local/share/callsheet";
const address = "127.0.0.1:4100";

const health: DaemonHealth = {
  captureContent: false,
  lastGlobalPosition: 12_408,
  erasurePending: false,
};

const running: RunningStatus = {
  ...base,
  state: "running",
  dataDir,
  address,
  pid: 7,
  daemonVersion: "0.1.0",
  schemaVersion: 3,
  uptimeMs: (2 * 60 + 14) * 60_000,
  health,
};

const REASONS: ErrorReason[] = [
  "no-data-dir",
  "unreadable",
  "invalid-record",
  "not-loopback",
  "bad-token",
  "protocol",
  "unexpected",
];

const EVERY_STATE: DaemonStatus[] = [
  { ...base, state: "connecting" },
  { ...base, state: "starting", dataDir, pid: 7 },
  { ...base, state: "not-running", dataDir, stale: false },
  { ...base, state: "not-running", dataDir, stale: true },
  running,
  { ...running, health: null },
  { ...running, health: { ...health, erasurePending: true } },
  { ...base, state: "unauthorized", dataDir, address },
  { ...base, state: "unreachable", dataDir, address, pid: 7 },
  ...REASONS.map(
    (reason): DaemonStatus => ({ ...base, state: "error", dataDir, reason }),
  ),
];

function strings(view: StatusView): string[] {
  return [
    view.headline,
    view.message,
    view.notice?.title,
    view.notice?.text,
    view.nextStep?.text,
    view.nextStep?.command,
    view.nextStep?.after,
    ...view.rows.flatMap((row) => [row.label, row.value, row.note]),
  ].filter((text): text is string => text !== undefined);
}

function row(view: StatusView, key: string) {
  return view.rows.find((r) => r.key === key);
}

test("every state has a headline and passes the main process's message on", () => {
  for (const status of EVERY_STATE) {
    const view = describeStatus(status);
    assert.ok(view.headline.length > 0, status.state);
    assert.equal(view.message, status.message);
  }
});

test("each state has its own headline and mark", () => {
  const expected: Record<DaemonStatus["state"], [string, string]> = {
    connecting: ["Checking for the daemon", "pending"],
    starting: ["Daemon starting", "pending"],
    "not-running": ["Daemon not running", "off"],
    running: ["Daemon running", "ok"],
    unauthorized: ["Daemon refused access", "warning"],
    unreachable: ["Daemon not responding", "danger"],
    error: ["Can't check the daemon", "danger"],
  };
  for (const status of EVERY_STATE) {
    const view = describeStatus(status);
    assert.deepEqual([view.headline, view.tone], expected[status.state]);
  }
});

test("every state that isn't healthy says what to do next", () => {
  for (const status of EVERY_STATE) {
    if (status.state === "running" || status.state === "connecting") {
      continue;
    }
    const step = describeStatus(status).nextStep;
    assert.ok(step && step.text.length > 0, JSON.stringify(status));
  }
});

test("not running: start cs-daemon, with --data-dir only for a custom directory", () => {
  const status: DaemonStatus = {
    ...base,
    state: "not-running",
    dataDir,
    stale: false,
  };
  assert.equal(describeStatus(status).nextStep?.command, "cs-daemon");
  assert.equal(
    describeStatus({ ...status, customDataDir: true }).nextStep?.command,
    `cs-daemon --data-dir ${dataDir}`,
  );
  assert.equal(row(describeStatus(status), "data-dir")?.value, dataDir);
});

test("running: aligned facts, humanised and grouped", () => {
  const view = describeStatus(running);
  assert.deepEqual(
    view.rows.map((r) => [r.label, r.value]),
    [
      ["Address", address],
      ["Version", "0.1.0"],
      ["Schema version", "3"],
      ["Uptime", "2\u00a0h 14\u00a0min"],
      ["Content capture", "Off"],
      ["Events recorded", (12_408).toLocaleString()],
    ],
  );
  assert.equal(row(view, "address")?.mono, true);
  assert.equal(row(view, "version")?.mono, undefined);
  assert.match(row(view, "capture")?.note ?? "", /never message content/);
  assert.equal(view.notice, null);
  assert.equal(view.nextStep, null);
});

test("content capture on says content is stored", () => {
  const view = describeStatus({
    ...running,
    health: { ...health, captureContent: true },
  });
  assert.equal(row(view, "capture")?.value, "On");
  assert.match(row(view, "capture")?.note ?? "", /stored on this machine/);
});

test("erasure pending is a warning, shown only when pending", () => {
  const pending = describeStatus({
    ...running,
    health: { ...health, erasurePending: true },
  });
  assert.equal(pending.notice?.tone, "warning");
  assert.equal(pending.notice?.title, "Erasure pending");
  assert.match(pending.notice?.text ?? "", /retries every 30 seconds/);
  assert.equal(describeStatus(running).notice, null);
});

test("without health: same rows (no layout shift), unknowns dimmed, and a note", () => {
  const partial = describeStatus({ ...running, health: null });
  assert.deepEqual(
    partial.rows.map((r) => r.key),
    describeStatus(running).rows.map((r) => r.key),
  );
  for (const key of ["capture", "events"]) {
    assert.deepEqual(
      [row(partial, key)?.value, row(partial, key)?.muted],
      ["Not reported", true],
    );
  }
  assert.equal(partial.notice?.tone, "info");
});

test("unauthorized names the token file, with the directory's own separator", () => {
  const unix = describeStatus({
    ...base,
    state: "unauthorized",
    dataDir,
    address,
  });
  assert.equal(row(unix, "token")?.value, `${dataDir}/control-token`);
  const windows = describeStatus({
    ...base,
    state: "unauthorized",
    dataDir: "C:\\Users\\ann\\AppData\\Roaming\\Callsheet\\data",
    address,
  });
  assert.equal(
    row(windows, "token")?.value,
    "C:\\Users\\ann\\AppData\\Roaming\\Callsheet\\data\\control-token",
  );
});

test("unreachable says which process to stop and how to start again", () => {
  const view = describeStatus({
    ...base,
    state: "unreachable",
    dataDir,
    address,
    pid: 4321,
  });
  assert.match(view.nextStep?.text ?? "", /stop process 4321/);
  assert.equal(view.nextStep?.command, "cs-daemon");
});

test("each error reason has its own next step", () => {
  const steps = REASONS.map(
    (reason) =>
      describeStatus({ ...base, state: "error", dataDir, reason }).nextStep
        ?.text,
  );
  assert.equal(new Set(steps).size, REASONS.length);
});

test("copy has no emoji, placeholder text or unexplained jargon", () => {
  for (const status of EVERY_STATE) {
    for (const text of strings(describeStatus(status))) {
      assert.doesNotMatch(text, /\p{Extended_Pictographic}/u, text);
      assert.doesNotMatch(
        text,
        /lorem|ipsum|TODO|TBD|undefined|null|NaN/i,
        text,
      );
      assert.doesNotMatch(text, /-32601|JSON-RPC|ECONN/, text);
    }
  }
});
