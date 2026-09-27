// The egress check's OS-level backstop (issue #56): runs a process under
// strace and finds every attempt to reach the network in its socket system
// calls, whether or not the process used a proxy. Used by `egress-check.ts`.
// Not shipped: the build only compiles src/.
//
// strace (strace.1, v6.8): `-f` follows fork, vfork and clone, so Chromium's
// child processes are traced too; `-s 0` prints no data ("filenames are not
// considered strings and are always printed in full", so socket paths still
// are); `-yy` names each socket's protocol; `-Y` prints each thread's command
// name. With `-o FILE PROG`, fatal signals are blocked in strace itself, and it
// exits with its command's status or signal, so stopping a traced process
// works as before.
//
// Under a tracer without privileges, "setuid and setgid programs are executed
// without effective privileges", and Chromium's setuid sandbox helper then
// aborts. So where that sandbox is in use (CI), strace runs as root through
// sudo and starts the command as the given user with `-u`, which strace
// documents for exactly this; the traced processes never run as root.

/** The system calls that can send a first packet to an address. */
export const TRACED_SYSCALLS = ["connect", "sendto", "sendmsg", "sendmmsg"];

/** Who the traced command runs as when strace itself runs as root. */
export interface TraceAs {
  user: string;
  home: string;
}

/**
 * strace and its arguments to run `command` with its socket calls logged.
 * With `as`, strace runs through `sudo` and starts the command as that user,
 * with that HOME; the rest of the environment is passed on unchanged.
 */
export function tracedCommand(
  traceFile: string,
  command: string,
  args: readonly string[],
  as: TraceAs | null = null,
): { command: string; args: string[] } {
  const strace = [
    "-f",
    // If strace itself is killed, its tracees die too: none is left running.
    "--kill-on-exit",
    "-qq",
    "-s",
    "0",
    // `-s` also caps how many array elements are printed; without this, a
    // sendmmsg vector prints as `[...]` and hides each message's address.
    // Strings stay empty either way.
    "-e",
    "abbrev=none",
    "-yy",
    "-Y",
    "-e",
    `trace=${TRACED_SYSCALLS.join(",")}`,
    "-o",
    traceFile,
  ];
  if (!as) {
    return { command: "strace", args: [...strace, "--", command, ...args] };
  }
  return {
    command: "sudo",
    args: [
      "--non-interactive",
      "--preserve-env",
      "strace",
      ...strace,
      "-u",
      as.user,
      "-E",
      `HOME=${as.home}`,
      "--",
      command,
      ...args,
    ],
  };
}

/** One socket address a traced thread connected or sent to. */
export interface SocketCall {
  /** Thread id and command name, as `-Y` prints them: `1234<electron>`. */
  thread: string;
  syscall: string;
  /** "TCP", "UDPv6", "UNIX-STREAM", … from `-yy`, or "?" when not shown. */
  protocol: string;
  /** "AF_INET", "AF_INET6", "AF_UNIX", … */
  family: string;
  /** An IP address or a socket path; empty when strace printed neither. */
  address: string;
  port: number | null;
}

/** Why a socket call counts as reaching beyond this machine. */
export type EgressReason =
  | "non-loopback address"
  | "DNS query to a local resolver"
  | "local resolver socket"
  | "other address family";

export interface Egress extends SocketCall {
  reason: EgressReason;
}

const LINE = /^(\d+(?:<[^>]*>)?)\s+([a-z0-9_]+)\((.*)$/;
const FD_PROTOCOL = /^\d+<([A-Za-z0-9-]+):/;
const FAMILY = /sa_family=(AF_[A-Z0-9_]+)/g;
const IPV4 = /^, sin_port=htons\((\d+)\), sin_addr=inet_addr\("([^"]*)"\)/;
const IPV6 =
  /^, sin6_port=htons\((\d+)\), sin6_flowinfo=[^,]*, inet_pton\(AF_INET6, "([^"]*)"/;
const UNIX_PATH = /^, sun_path=(@?)"((?:[^"\\]|\\.)*)"/;

/** The families that carry no traffic off the machine by themselves. */
const LOCAL_FAMILIES = new Set(["AF_UNIX", "AF_NETLINK", "AF_UNSPEC"]);
/** Sockets through which a process asks a local daemon to resolve a name. */
const RESOLVER_SOCKETS = [
  "/run/systemd/resolve/",
  "/run/avahi-daemon/socket",
  "/var/run/avahi-daemon/socket",
];
const DNS_PORT = 53;

/**
 * The socket addresses in one line of `strace -f -yy -Y` output. A call made
 * without an address (a send on a connected socket) has none; its `connect`
 * was traced when the socket was connected.
 */
export function parseTraceLine(line: string): SocketCall[] {
  const match = LINE.exec(line);
  if (!match || !TRACED_SYSCALLS.includes(match[2])) {
    return [];
  }
  const [, thread, syscall, rest] = match;
  const protocol = FD_PROTOCOL.exec(rest)?.[1] ?? "?";
  const calls: SocketCall[] = [];
  for (const family of rest.matchAll(FAMILY)) {
    const after = rest.slice((family.index ?? 0) + family[0].length);
    const call = { thread, syscall, protocol, family: family[1] };
    const ipv4 = IPV4.exec(after);
    const ipv6 = IPV6.exec(after);
    const unix = UNIX_PATH.exec(after);
    if (ipv4) {
      calls.push({ ...call, address: ipv4[2], port: Number(ipv4[1]) });
    } else if (ipv6) {
      calls.push({ ...call, address: ipv6[2], port: Number(ipv6[1]) });
    } else if (unix) {
      calls.push({ ...call, address: unix[1] + unix[2], port: null });
    } else {
      calls.push({ ...call, address: "", port: null });
    }
  }
  return calls;
}

/**
 * True for an address that stays on this host: 127.0.0.0/8, ::1, their
 * IPv4-mapped forms, and the unspecified addresses, which Linux connects to
 * the local host.
 */
export function isLocalAddress(address: string): boolean {
  const lower = address.toLowerCase();
  const v4 = lower.startsWith("::ffff:") ? lower.slice(7) : lower;
  return (
    /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(v4) ||
    v4 === "0.0.0.0" ||
    lower === "::1" ||
    lower === "::"
  );
}

/** Why `call` reaches beyond this machine, or null when it doesn't. */
export function egressReason(call: SocketCall): EgressReason | null {
  switch (call.family) {
    case "AF_INET":
    case "AF_INET6":
      if (!isLocalAddress(call.address)) {
        return "non-loopback address";
      }
      // A local stub resolver (127.0.0.53 on Ubuntu) forwards the query.
      return call.port === DNS_PORT ? "DNS query to a local resolver" : null;
    case "AF_UNIX":
      return RESOLVER_SOCKETS.some((socket) => call.address.startsWith(socket))
        ? "local resolver socket"
        : null;
    default:
      return LOCAL_FAMILIES.has(call.family) ? null : "other address family";
  }
}

/** Every socket call in a trace, in order. */
export function socketCalls(trace: string): SocketCall[] {
  return trace.split("\n").flatMap(parseTraceLine);
}

/** The socket calls in a trace that reach beyond this machine. */
export function findEgress(trace: string): Egress[] {
  return socketCalls(trace).flatMap((call) => {
    const reason = egressReason(call);
    return reason ? [{ ...call, reason }] : [];
  });
}

/** "TCP 93.184.215.14:443", "UNIX-STREAM /run/…", for reports. */
export function describeDestination(call: SocketCall): string {
  if (call.port === null) {
    return `${call.protocol} ${call.address || call.family}`;
  }
  const host = call.address.includes(":") ? `[${call.address}]` : call.address;
  return `${call.protocol} ${host}:${call.port}`;
}
