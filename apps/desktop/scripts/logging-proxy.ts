// A loopback HTTP proxy that forwards nothing: it records where each request
// tried to go and answers 403. Used by `egress-check.ts` (issue #56). Not
// shipped: the build only compiles src/.
//
// Only the method and the destination (host:port) of the request line are kept.
// Paths, query strings, headers and bodies are never stored or printed, so a
// credential in a request can't reach the CI log.
import net from "node:net";

/** One refused request: who sent it, when, and where it tried to go. */
export interface Attempt {
  /** Which logging proxy saw it, which names the process that sent it. */
  process: string;
  /** "first start", "restart", "idle start", … */
  phase: string;
  /** The request method, or "(unparsed)" for bytes that aren't a request line. */
  method: string;
  /** `host:port`, or "(unparsed)". */
  target: string;
}

/** Requests that reached the proxy, grouped by process, phase and destination. */
export interface Tally {
  process: string;
  phase: string;
  target: string;
  count: number;
}

export const UNPARSED = "(unparsed)";
/** The longest request line read before the request counts as unparsed. */
export const MAX_REQUEST_LINE_BYTES = 8 * 1024;
/** How long a connection may stay open without a complete request line. */
export const REQUEST_LINE_TIMEOUT_MS = 5000;

const REFUSAL =
  "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const DEFAULT_PORTS: Record<string, string> = {
  "http:": "80",
  "https:": "443",
  "ws:": "80",
  "wss:": "443",
  "ftp:": "21",
};
// RFC 9110 token characters, for the method.
const METHOD = /^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,32}$/;
// A host name, an IPv4 literal or a bracketed IPv6 literal, then a port.
const AUTHORITY = /^(\[[0-9A-Fa-f:.]{2,45}\]|[0-9A-Za-z.-]{1,253}):(\d{1,5})$/;

/**
 * The method and `host:port` of a proxy request line, or null when it isn't
 * one. CONNECT carries `host:port` (authority form); other methods carry an
 * absolute URL, whose path and query are dropped here.
 */
export function parseRequestLine(
  line: string,
): { method: string; target: string } | null {
  const parts = line.split(" ");
  if (parts.length !== 3 || !/^HTTP\/\d(\.\d)?$/.test(parts[2])) {
    return null;
  }
  const [method, target] = parts;
  if (!METHOD.test(method)) {
    return null;
  }
  if (method === "CONNECT") {
    const match = AUTHORITY.exec(target);
    return match && Number(match[2]) <= 65535
      ? { method, target: target.toLowerCase() }
      : null;
  }
  let url: URL;
  try {
    url = new URL(target);
  } catch {
    return null;
  }
  const port = url.port || DEFAULT_PORTS[url.protocol];
  if (!url.hostname || !port) {
    return null;
  }
  return { method, target: `${url.hostname}:${port}` };
}

/** Groups attempts by process, phase and destination, in first-seen order. */
export function tally(attempts: readonly Attempt[]): Tally[] {
  const groups = new Map<string, Tally>();
  for (const { process, phase, target } of attempts) {
    const key = JSON.stringify([process, phase, target]);
    const group = groups.get(key);
    if (group) {
      group.count += 1;
    } else {
      groups.set(key, { process, phase, target, count: 1 });
    }
  }
  return [...groups.values()];
}

/**
 * Splits attempts into those an explicit allowlist of `host:port` entries
 * permits and those it doesn't. The idle-start check passes no allowlist.
 */
export function partition(
  attempts: readonly Attempt[],
  allowlist: readonly string[] = [],
): { allowed: Attempt[]; refused: Attempt[] } {
  const allowed: Attempt[] = [];
  const refused: Attempt[] = [];
  const permitted = new Set(allowlist.map((entry) => entry.toLowerCase()));
  for (const attempt of attempts) {
    (permitted.has(attempt.target) ? allowed : refused).push(attempt);
  }
  return { allowed, refused };
}

/**
 * A proxy on `127.0.0.1` that refuses every request with 403 and records it
 * under the current {@link LoggingProxy.phase}. Nothing is ever forwarded.
 */
export class LoggingProxy {
  /** Names who is sent to this proxy, for the report. */
  readonly process: string;
  readonly attempts: Attempt[] = [];
  /**
   * Errors the listening server reported after it started, such as failing
   * to accept a connection. A proxy that stopped accepting could miss a
   * request, so the check fails on any.
   */
  readonly errors: string[] = [];
  phase = "";
  #port = 0;
  #listening = false;
  readonly #sockets = new Set<net.Socket>();
  readonly #server = net.createServer((socket) => this.#accept(socket));

  constructor(process: string) {
    this.process = process;
    this.#server.on("error", (error) => {
      if (this.#listening) {
        this.errors.push(error.message);
      }
    });
  }

  /** Listens on an unused loopback port. */
  async listen(): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      this.#server.once("error", reject);
      this.#server.listen(0, "127.0.0.1", () => {
        this.#server.off("error", reject);
        resolve();
      });
    });
    this.#port = (this.#server.address() as net.AddressInfo).port;
    this.#listening = true;
  }

  /** `127.0.0.1:<port>`, for `--proxy-server`. */
  get address(): string {
    return `127.0.0.1:${this.#port}`;
  }

  /** `http://127.0.0.1:<port>`, for `HTTPS_PROXY` and `HTTP_PROXY`. */
  get url(): string {
    return `http://${this.address}`;
  }

  /** Stops listening and drops open connections. */
  async close(): Promise<void> {
    for (const socket of this.#sockets) {
      socket.destroy();
    }
    await new Promise<void>((resolve) => this.#server.close(() => resolve()));
  }

  #accept(socket: net.Socket): void {
    this.#sockets.add(socket);
    const phase = this.phase;
    let received = Buffer.alloc(0);
    let done = false;
    const record = (method: string, target: string) => {
      if (!done) {
        done = true;
        this.attempts.push({ process: this.process, phase, method, target });
      }
    };
    const refuse = (method: string, target: string) => {
      record(method, target);
      socket.removeAllListeners("data");
      socket.end(REFUSAL);
    };

    socket.setTimeout(REQUEST_LINE_TIMEOUT_MS, () => {
      if (received.length > 0) {
        record(UNPARSED, UNPARSED);
      }
      socket.destroy();
    });
    socket.on("data", (chunk: Buffer) => {
      received = Buffer.concat([received, chunk]);
      const end = received.indexOf("\n");
      if (end === -1) {
        if (received.length > MAX_REQUEST_LINE_BYTES) {
          refuse(UNPARSED, UNPARSED);
        }
        return;
      }
      const line = received
        .subarray(0, Math.min(end, MAX_REQUEST_LINE_BYTES))
        .toString("latin1")
        .replace(/\r$/, "");
      const parsed = end <= MAX_REQUEST_LINE_BYTES && parseRequestLine(line);
      if (parsed) {
        refuse(parsed.method, parsed.target);
      } else {
        refuse(UNPARSED, UNPARSED);
      }
    });
    socket.on("end", () => {
      // Bytes without a complete line still count; a connection that sent
      // nothing (a speculative preconnect) is not a request.
      if (received.length > 0) {
        record(UNPARSED, UNPARSED);
      }
    });
    socket.on("error", () => {});
    socket.on("close", () => this.#sockets.delete(socket));
  }
}
