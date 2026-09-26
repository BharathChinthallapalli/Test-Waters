# 0003. Control API is JSON-RPC 2.0 over loopback HTTP with a per-install token

- Status: Accepted
- Date: 2026-09-26

## Context

The desktop app and the CLI talk to the daemon ([ADR 0001](0001-daemon-not-sidecar.md)).
The API must work the same from Rust and TypeScript, carry typed requests and
errors, and be safe on a machine where web pages and other local programs can
reach 127.0.0.1.

Options considered: REST, gRPC, JSON-RPC 2.0, and OS-specific IPC (Unix domain
sockets, Windows named pipes). JSON-RPC 2.0 is small, transport-agnostic and
already used by the protocols Callsheet speaks (MCP and ACP both use it), so
one error model and one envelope serve every boundary. OS-specific IPC would
give file-permission access control but needs a separate implementation per
platform.

A loopback listener is still reachable from a browser through DNS rebinding.
The MCP specification's guidance for local HTTP servers addresses the same
threat: validate `Origin`, bind only to 127.0.0.1, and authenticate every
connection.

## Decision

- The control API is JSON-RPC 2.0 carried over HTTP on 127.0.0.1 only.
- Each install generates a random token at first start, stored in a file
  readable only by the owning OS user. Every request must present it as
  `Authorization: Bearer <token>`; the daemon compares it in constant time
  and never logs it.
- In the desktop app, only the Electron main process reads the token file and
  calls the daemon. The renderer never sees the token; it reaches the daemon
  only through the narrow preload API.
- The daemon rejects requests whose `Origin` header is present and not an
  allowed Callsheet client, and requests whose `Host` header is not exactly
  `127.0.0.1:<bound port>`. Clients connect by that address, not by
  `localhost`, so no other `Host` value is accepted.
- Method names, parameters and errors are Rust types in `cs-core`, exported to
  TypeScript (feature 01, task 4). The first methods are `health` and
  `version` (feature 02).

## Consequences

- Any local process running as the same user can read the token file and call
  the API. This is accepted: that user can already read the database. It is
  stated in the threat model rather than hidden.
- The token must be rotatable, and clients must handle an auth failure after
  rotation.
- Error codes follow JSON-RPC 2.0: -32768 to -32000 is reserved, and
  application errors use codes outside it.
- Streaming responses (for example, a live timeline) need a design on top of
  HTTP; this is deferred until a feature needs it.

## Sources

- JSON-RPC 2.0 Specification: https://www.jsonrpc.org/specification (read
  via the Context7 index of jsonrpc.org).
- MCP specification 2026-07-28, Streamable HTTP transport, security warning
  on DNS rebinding:
  https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http
  (source: https://github.com/modelcontextprotocol/modelcontextprotocol/blob/2026-07-28/docs/specification/2026-07-28/basic/transports/streamable-http.mdx,
  tag `2026-07-28`, commit `5f5440bb26a62e2cf3440b92da5a667efa03b267`).
