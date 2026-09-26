# Security policy

## Reporting a vulnerability

Please report security problems **privately**. Do not open a public issue,
discussion or pull request that describes the problem.

The repository is currently **private**, so GitHub's private vulnerability
reporting is not available yet (issue #28). Until the repository is public:

1. Open an issue titled **"Private security contact request"** containing no
   details of the problem.
2. The maintainer ([@BharathChinthallapalli](https://github.com/BharathChinthallapalli))
   will reply with a private channel for the details.

When the repository becomes public, private vulnerability reporting will be
enabled and this section will point to the repository's **Security → Report a
vulnerability** form instead.

Please include what you found, how to reproduce it, and which version or
commit you tested. You will get an acknowledgement, and a fix or an
explanation, as quickly as a one-person project allows.

## Supported versions

Callsheet is pre-alpha and has no releases yet. Only the `main` branch is
supported.

## Local control API

The Callsheet daemon (feature 02, being built) is controlled through a small
JSON-RPC API ([ADR 0003](docs/adr/0003-json-rpc-control-api.md)). It can be
used only by programs on your machine that can read your token:

- It listens on `127.0.0.1` only. The daemon refuses to start with any other
  listen address.
- Every request needs a bearer token. The token is random (256 bits), kept in
  a file only your user account can read, and never written to logs.
- Any request with an `Origin` header, or with a `Host` header other than
  exactly `127.0.0.1:<port>`, is rejected, so web pages open in a browser
  can't use the API.
- The token can be rotated (the `token.rotate` call; there is no command-line
  or desktop control for it yet). The old token stops working at once.

## Scope and design limits

Callsheet runs locally. Some limits are deliberate and documented rather than
hidden:

- Any process running as the same operating-system user can read the local
  control-API token ([ADR 0003](docs/adr/0003-json-rpc-control-api.md)).
- The event log is **tamper-evident, not tamper-proof**
  ([ADR 0007](docs/adr/0007-identity-and-log-integrity.md)). Each run's events
  are hash-chained and every event records its place in the daemon's global
  commit order, so verification detects an event that was edited, reordered
  or inserted, or removed from anywhere except the end of the global commit
  order, unless every later hash is recomputed (see below).
- Until signed checkpoints exist (feature 04), a hash chain has no outside
  anchor to compare against. So **removing the most recent events** (the end
  of the global commit order) cannot be detected, and someone who can write
  the database can change events and recompute every hash after them without
  verification noticing.
- Once checkpoints exist, someone with full control of the machine and its
  keychain can still rewrite history, but not without breaking checkpoints
  that were already exported.

Reports that show these limits being worse than documented are in scope.
