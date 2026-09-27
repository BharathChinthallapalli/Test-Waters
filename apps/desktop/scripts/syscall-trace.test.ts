import assert from "node:assert/strict";
import { test } from "node:test";
import {
  describeDestination,
  egressReason,
  findEgress,
  isLocalAddress,
  parseTraceLine,
  type SocketCall,
  tracedCommand,
} from "./syscall-trace.ts";

// Lines as strace 6.8 prints them with -f -qq -s 0 -e abbrev=none -yy -Y
// (older lines, recorded without abbrev=none, differ only in how much of an
// iovec they print). The IPv4, UDP and UNIX lines were recorded from Node and
// Electron; the IPv6 line and the unconnected sendmmsg follow the same
// decoders' formats.
const TCP4_UNFINISHED =
  '18321<node> connect(21<TCP:[288682]>, {sa_family=AF_INET, sin_port=htons(9), sin_addr=inet_addr("192.0.2.1")}, 16 <unfinished ...>';
const TCP4_RESUMED =
  "18321<node> <... connect resumed>)      = -1 EINPROGRESS (Operation now in progress)";
const TCP4_LOOPBACK =
  '18321<node> connect(23<TCP:[290494]>, {sa_family=AF_INET, sin_port=htons(9), sin_addr=inet_addr("127.0.0.1")}, 16) = -1 EINPROGRESS (Operation now in progress)';
const UDP_DNS_REMOTE =
  '18328<libuv-worker> connect(22<UDP:[289713]>, {sa_family=AF_INET, sin_port=htons(53), sin_addr=inet_addr("8.8.8.8")}, 16) = 0';
const UDP_SENDMMSG_CONNECTED =
  '583<libuv-worker> sendmmsg(21<UDP:[345454]>, [{msg_hdr={msg_name=NULL, msg_namelen=0, msg_iov=[{iov_base=""..., iov_len=32}], msg_iovlen=1, msg_controllen=0, msg_flags=0}, msg_len=32}, {msg_hdr={msg_name=NULL, msg_namelen=0, msg_iov=[{iov_base=""..., iov_len=32}], msg_iovlen=1, msg_controllen=0, msg_flags=0}, msg_len=32}], 2, MSG_NOSIGNAL) = 2';
// An unconnected socket: each message names its destination.
const UDP_SENDMMSG_UNCONNECTED =
  '90<quic> sendmmsg(7<UDP:[1]>, [{msg_hdr={msg_name={sa_family=AF_INET, sin_port=htons(443), sin_addr=inet_addr("198.51.100.7")}, msg_namelen=16, msg_iov=[{iov_base=""..., iov_len=1200}], msg_iovlen=1, msg_controllen=0, msg_flags=0}, msg_len=1200}, {msg_hdr={msg_name={sa_family=AF_INET, sin_port=htons(53), sin_addr=inet_addr("127.0.0.1")}, msg_namelen=16, msg_iov=[{iov_base=""..., iov_len=40}], msg_iovlen=1, msg_controllen=0, msg_flags=0}, msg_len=40}], 2, 0) = 2';
const UDP_DNS_STUB =
  '18321<node> sendmsg(24<UDP:[290495]>, {msg_name={sa_family=AF_INET, sin_port=htons(53), sin_addr=inet_addr("127.0.0.53")}, msg_namelen=16, msg_iov=[...], msg_iovlen=1, msg_controllen=0, msg_flags=0}, 0) = 1';
const TCP6 =
  '4242<NetworkServic> connect(31<TCPv6:[5555]>, {sa_family=AF_INET6, sin6_port=htons(443), sin6_flowinfo=htonl(0), inet_pton(AF_INET6, "2001:db8::1", &sin6_addr), sin6_scope_id=0}, 28) = -1 ENETUNREACH (Network is unreachable)';
const X11 =
  '12710<electron> connect(34<UNIX-STREAM:[279643]>, {sa_family=AF_UNIX, sun_path=@"/tmp/.X11-unix/X100"}, 22) = 0';
const DBUS =
  '12717<ThreadPoolSingl> connect(18<UNIX-STREAM:[279638]>, {sa_family=AF_UNIX, sun_path="/run/dbus/system_bus_socket"}, 29) = -1 ENOENT (No such file or directory)';
const RESOLVED =
  '77<node> connect(9<UNIX-STREAM:[1]>, {sa_family=AF_UNIX, sun_path="/run/systemd/resolve/io.systemd.Resolve"}, 42) = 0';
const NETLINK =
  "12716<ThreadPoolForeg> sendto(42<NETLINK:[279645]>, ..., 17, 0, {sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}, 12) = 17";
const IPC =
  '12744<electron> sendto(13<UNIX-STREAM:[278921]>, ""..., 80, MSG_NOSIGNAL, NULL, 0) = 80';
const PACKET =
  "9<x> sendto(3<PACKET:[1]>, ..., 42, 0, {sa_family=AF_PACKET, sll_protocol=htons(ETH_P_IP), sll_ifindex=2}, 20) = 42";

test("strace runs the command with the socket calls traced into a file", () => {
  const { command, args } = tracedCommand("/t/trace", "/bin/app", ["--x"]);
  assert.equal(command, "strace");
  assert.deepEqual(args.slice(-4), ["/t/trace", "--", "/bin/app", "--x"]);
  assert.ok(args.includes("-f"), "follows child processes");
  assert.ok(args.includes("--kill-on-exit"), "leaves no tracee behind");
  assert.ok(args.includes("abbrev=none"), "prints every sendmmsg message");
  assert.deepEqual(args.slice(args.indexOf("-s"), args.indexOf("-s") + 2), [
    "-s",
    "0",
  ]);
  assert.ok(args.includes("trace=connect,sendto,sendmsg,sendmmsg"));
});

test("with a user, strace runs through sudo and starts the command as that user", () => {
  const { command, args } = tracedCommand("/t/trace", "/bin/app", ["--x"], {
    user: "runner",
    home: "/home/runner",
  });
  assert.equal(command, "sudo");
  assert.deepEqual(args.slice(0, 3), [
    "--non-interactive",
    "--preserve-env",
    "strace",
  ]);
  assert.deepEqual(args.slice(-7), [
    "-u",
    "runner",
    "-E",
    "HOME=/home/runner",
    "--",
    "/bin/app",
    "--x",
  ]);
});

test("an IPv4 connect gives thread, protocol, address and port", () => {
  assert.deepEqual(parseTraceLine(TCP4_UNFINISHED), [
    {
      thread: "18321<node>",
      syscall: "connect",
      protocol: "TCP",
      family: "AF_INET",
      address: "192.0.2.1",
      port: 9,
    },
  ]);
});

test("an IPv6 connect gives its address and port", () => {
  const [call] = parseTraceLine(TCP6);
  assert.equal(call.family, "AF_INET6");
  assert.equal(call.address, "2001:db8::1");
  assert.equal(call.port, 443);
  assert.equal(describeDestination(call), "TCPv6 [2001:db8::1]:443");
});

test("sendmsg's msg_name and a UNIX path are read", () => {
  assert.deepEqual(
    parseTraceLine(UDP_DNS_STUB).map((c) => [c.address, c.port]),
    [["127.0.0.53", 53]],
  );
  assert.deepEqual(
    parseTraceLine(X11).map((c) => [c.family, c.address]),
    [["AF_UNIX", "@/tmp/.X11-unix/X100"]],
  );
});

test("each message of a sendmmsg to an unconnected socket is read", () => {
  assert.deepEqual(
    parseTraceLine(UDP_SENDMMSG_UNCONNECTED).map((c) => [
      describeDestination(c),
      egressReason(c),
    ]),
    [
      ["UDP 198.51.100.7:443", "non-loopback address"],
      ["UDP 127.0.0.1:53", "DNS query to a local resolver"],
    ],
  );
});

test("resumed lines, sends without an address and other lines give nothing", () => {
  for (const line of [
    TCP4_RESUMED,
    UDP_SENDMMSG_CONNECTED,
    IPC,
    "18321<node> +++ exited with 0 +++",
    "18321<node> --- SIGTERM {si_signo=SIGTERM, si_code=SI_USER} ---",
    '18321<node> openat(AT_FDCWD, "/etc/hosts", O_RDONLY) = 3',
    "",
  ]) {
    assert.deepEqual(parseTraceLine(line), [], line);
  }
});

test("loopback and unspecified addresses are local; others are not", () => {
  for (const address of [
    "127.0.0.1",
    "127.0.0.53",
    "127.255.0.9",
    "0.0.0.0",
    "::1",
    "::",
    "::ffff:127.0.0.1",
  ]) {
    assert.ok(isLocalAddress(address), address);
  }
  for (const address of [
    "8.8.8.8",
    "169.254.169.254",
    "10.0.0.1",
    "192.0.2.1",
    "2001:db8::1",
    "fe80::1",
    "::ffff:93.184.215.14",
    "128.0.0.1",
  ]) {
    assert.ok(!isLocalAddress(address), address);
  }
});

test("egress: remote addresses, DNS to a local stub, resolver sockets, raw families", () => {
  const reasons = [
    TCP4_UNFINISHED,
    UDP_DNS_REMOTE,
    UDP_DNS_STUB,
    TCP6,
    RESOLVED,
    PACKET,
  ].map((line) => parseTraceLine(line).map(egressReason));
  assert.deepEqual(reasons, [
    ["non-loopback address"],
    ["non-loopback address"],
    ["DNS query to a local resolver"],
    ["non-loopback address"],
    ["local resolver socket"],
    ["other address family"],
  ]);
});

test("not egress: loopback, X11, D-Bus, netlink", () => {
  for (const line of [TCP4_LOOPBACK, X11, DBUS, NETLINK]) {
    const [call] = parseTraceLine(line);
    assert.equal(egressReason(call as SocketCall), null, line);
  }
});

test("findEgress keeps only the calls that leave the machine", () => {
  const trace = [
    X11,
    TCP4_LOOPBACK,
    TCP4_UNFINISHED,
    TCP4_RESUMED,
    IPC,
    UDP_DNS_STUB,
  ].join("\n");
  assert.deepEqual(
    findEgress(trace).map((e) => [describeDestination(e), e.reason]),
    [
      ["TCP 192.0.2.1:9", "non-loopback address"],
      ["UDP 127.0.0.53:53", "DNS query to a local resolver"],
    ],
  );
});
