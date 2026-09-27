// What the egress check's canary (`egress-canary.ts`) tries to reach, and so
// what the check expects each backstop to see. Shared by the canary, which
// runs in Electron's main process, and `egress-check.ts`. Not shipped.
//
// Host names end in `.invalid` (RFC 6761), so no request to them can succeed.
// IP addresses are in TEST-NET-1, 192.0.2.0/24 (RFC 5737), which is never
// routed; the canary's direct connections there are refused or time out.

export const CANARY = {
  /** Main-process Node `fetch`: must reach the proxy named by HTTPS_PROXY. */
  nodeFetchUrl: "https://canary-node.invalid/",
  nodeFetchTarget: "canary-node.invalid:443",
  /** `net.fetch` in the default session: must reach `--proxy-server`. */
  chromiumFetchUrl: "https://canary-chromium.invalid/",
  chromiumFetchTarget: "canary-chromium.invalid:443",
  /** `fetch` in a separate session partition: must reach `--proxy-server`. */
  partitionFetchUrl: "https://canary-partition.invalid/",
  partitionFetchTarget: "canary-partition.invalid:443",
  /**
   * A link-local address, which Chromium sends directly unless
   * `--proxy-bypass-list=<-loopback>` removes its implicit bypass rules.
   */
  linkLocalUrl: "http://169.254.0.9/",
  linkLocalTarget: "169.254.0.9:80",
  /** A raw TCP connection from main-process Node: must show in the trace. */
  nodeSocketHost: "192.0.2.1",
  nodeSocketPort: 9,
  /**
   * A fetch from a session set to connect directly, so Chromium's network
   * service (a child process) connects: must show in the trace. Port 443,
   * since Chromium refuses port 9 (net::ERR_UNSAFE_PORT).
   */
  directFetchUrl: "https://192.0.2.2/",
  directHost: "192.0.2.2",
  directPort: 443,
  /** A name lookup through the C library: must show in the trace as DNS. */
  lookupHost: "canary-dns.invalid",
} as const;
