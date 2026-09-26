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

## Scope and design limits

Callsheet runs locally. Some limits are deliberate and documented rather than
hidden:

- Any process running as the same operating-system user can read the local
  control-API token ([ADR 0003](docs/adr/0003-json-rpc-control-api.md)).
- The event log is **tamper-evident, not tamper-proof**: someone with full
  control of the machine and its keychain can rewrite history, but not without
  breaking checkpoints that were already exported
  ([ADR 0007](docs/adr/0007-identity-and-log-integrity.md)).

Reports that show these limits being worse than documented are in scope.
