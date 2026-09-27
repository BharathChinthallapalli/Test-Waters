import assert from "node:assert/strict";
import net from "node:net";
import { after, before, test } from "node:test";
import {
  type Attempt,
  LoggingProxy,
  MAX_REQUEST_LINE_BYTES,
  parseRequestLine,
  partition,
  tally,
  UNPARSED,
} from "./logging-proxy.ts";

test("a CONNECT line gives its host:port", () => {
  assert.deepEqual(
    parseRequestLine("CONNECT redirector.gvt1.com:443 HTTP/1.1"),
    { method: "CONNECT", target: "redirector.gvt1.com:443" },
  );
  assert.deepEqual(parseRequestLine("CONNECT [::1]:8443 HTTP/1.1"), {
    method: "CONNECT",
    target: "[::1]:8443",
  });
  assert.deepEqual(parseRequestLine("CONNECT Example.COM:443 HTTP/1.1"), {
    method: "CONNECT",
    target: "example.com:443",
  });
});

test("an absolute URL gives host:port and drops credentials, path and query", () => {
  assert.deepEqual(
    parseRequestLine(
      "GET http://user:secret@example.com/path?key=sk-ant-123 HTTP/1.1",
    ),
    { method: "GET", target: "example.com:80" },
  );
  assert.deepEqual(
    parseRequestLine("POST https://api.example.com:8443/v1/messages HTTP/1.1"),
    { method: "POST", target: "api.example.com:8443" },
  );
  assert.deepEqual(parseRequestLine("GET ws://127.0.0.1:9/x HTTP/1.1"), {
    method: "GET",
    target: "127.0.0.1:9",
  });
});

test("anything that isn't a proxy request line is rejected", () => {
  for (const line of [
    "",
    "GET / HTTP/1.1", // origin form: no destination in the line
    "CONNECT example.com HTTP/1.1", // no port
    "CONNECT example.com:99999 HTTP/1.1",
    "CONNECT user:pw@example.com:443 HTTP/1.1",
    "CONNECT example.com:443",
    "GET http://example.com/ HTTP/1.1 extra",
    "GéT http://example.com/ HTTP/1.1",
    "GET mailto:someone@example.com HTTP/1.1",
    "\u0016\u0003\u0001\u0002\u0000\u0001",
  ]) {
    assert.equal(parseRequestLine(line), null, JSON.stringify(line));
  }
});

const attempt = (process: string, phase: string, target: string): Attempt => ({
  process,
  phase,
  method: "CONNECT",
  target,
});

test("tally counts by process, phase and destination in first-seen order", () => {
  assert.deepEqual(
    tally([
      attempt("app", "first start", "a.example:443"),
      attempt("app", "restart", "a.example:443"),
      attempt("app", "first start", "a.example:443"),
      attempt("daemon", "idle start", "a.example:443"),
      attempt("app", "first start", "b.example:443"),
    ]),
    [
      {
        process: "app",
        phase: "first start",
        target: "a.example:443",
        count: 2,
      },
      { process: "app", phase: "restart", target: "a.example:443", count: 1 },
      {
        process: "daemon",
        phase: "idle start",
        target: "a.example:443",
        count: 1,
      },
      {
        process: "app",
        phase: "first start",
        target: "b.example:443",
        count: 1,
      },
    ],
  );
});

test("with no allowlist every attempt is refused; an allowlist entry matches host:port exactly", () => {
  const attempts = [
    attempt("app", "p", "api.anthropic.com:443"),
    attempt("app", "p", "api.anthropic.com:80"),
    attempt("app", "p", UNPARSED),
  ];
  assert.deepEqual(partition(attempts).refused, attempts);
  assert.deepEqual(partition(attempts).allowed, []);
  const split = partition(attempts, ["API.anthropic.com:443"]);
  assert.deepEqual(split.allowed, [attempts[0]]);
  assert.deepEqual(split.refused, [attempts[1], attempts[2]]);
});

// The proxy itself, over real loopback sockets.
const proxy = new LoggingProxy("test client");
before(() => proxy.listen());
after(() => proxy.close());

/** Sends `bytes`, half-closes, and returns everything the proxy answered. */
function exchange(bytes: string | Buffer): Promise<string> {
  const [host, port] = proxy.address.split(":");
  return new Promise((resolve, reject) => {
    const socket = net.connect(Number(port), host, () => socket.end(bytes));
    const chunks: Buffer[] = [];
    socket.on("data", (chunk: Buffer) => chunks.push(chunk));
    socket.on("error", reject);
    socket.on("close", () => resolve(Buffer.concat(chunks).toString("latin1")));
  });
}

test("the proxy listens on loopback only", () => {
  assert.match(proxy.address, /^127\.0\.0\.1:\d+$/);
  assert.equal(proxy.url, `http://${proxy.address}`);
});

test("every request is refused with 403 and recorded under the current phase, without headers", async () => {
  proxy.attempts.length = 0;
  proxy.phase = "first start";
  const connect = await exchange(
    "CONNECT redirector.gvt1.com:443 HTTP/1.1\r\nHost: redirector.gvt1.com:443\r\nProxy-Authorization: Basic c2VjcmV0\r\n\r\n",
  );
  proxy.phase = "restart";
  const get = await exchange(
    "GET http://example.com/a?token=abc HTTP/1.1\r\nAuthorization: Bearer abc\r\n\r\n",
  );

  for (const answer of [connect, get]) {
    assert.match(answer, /^HTTP\/1\.1 403 Forbidden\r\n/);
    assert.doesNotMatch(answer, /200/);
  }
  assert.deepEqual(proxy.attempts, [
    {
      process: "test client",
      phase: "first start",
      method: "CONNECT",
      target: "redirector.gvt1.com:443",
    },
    {
      process: "test client",
      phase: "restart",
      method: "GET",
      target: "example.com:80",
    },
  ]);
  const recorded = JSON.stringify(proxy.attempts);
  for (const secret of ["c2VjcmV0", "Bearer", "token=abc", "/a"]) {
    assert.equal(recorded.includes(secret), false, secret);
  }
});

test("bytes that aren't a request line still count, as unparsed", async () => {
  proxy.attempts.length = 0;
  proxy.phase = "p";
  // A TLS ClientHello sent straight to the proxy, with no line ending.
  await exchange(Buffer.from([0x16, 0x03, 0x01, 0x00, 0x05, 1, 2, 3, 4, 5]));
  // A line that is too long.
  const long = await exchange(
    `GET http://${"a".repeat(MAX_REQUEST_LINE_BYTES)}`,
  );
  // A malformed line.
  await exchange("HELLO\r\n\r\n");
  assert.match(long, /^HTTP\/1\.1 403/);
  assert.deepEqual(
    proxy.attempts.map((a) => [a.method, a.target]),
    [
      [UNPARSED, UNPARSED],
      [UNPARSED, UNPARSED],
      [UNPARSED, UNPARSED],
    ],
  );
});

test("a connection that sends nothing is not a request", async () => {
  proxy.attempts.length = 0;
  await exchange("");
  assert.deepEqual(proxy.attempts, []);
});

test("a request line split across packets is read whole", async () => {
  proxy.attempts.length = 0;
  const [host, port] = proxy.address.split(":");
  const answer = await new Promise<string>((resolve, reject) => {
    const socket = net.connect(Number(port), host, () => {
      socket.write("CONNECT example");
      setTimeout(() => socket.write(".org:443 HTTP/1.1\r\n\r\n"), 50);
    });
    const chunks: Buffer[] = [];
    socket.on("data", (chunk: Buffer) => chunks.push(chunk));
    socket.on("error", reject);
    socket.on("close", () => resolve(Buffer.concat(chunks).toString("latin1")));
  });
  assert.match(answer, /^HTTP\/1\.1 403/);
  assert.deepEqual(
    proxy.attempts.map((a) => a.target),
    ["example.org:443"],
  );
});
