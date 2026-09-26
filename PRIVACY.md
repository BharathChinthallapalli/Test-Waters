# Privacy

Callsheet is local-first. **Nothing leaves your machine by default.** This file
is the contract; if the code ever disagrees with it, the code is the bug.

## What Callsheet sends

- **No telemetry.** Callsheet sends no usage data, crash reports or analytics.
  If telemetry is ever added it will be **opt-in**: off until you turn it on,
  described here first, and switchable off at any time.
- **Your own model traffic.** When you route an agent through the Callsheet
  proxy, the proxy forwards that agent's requests to the provider you
  configured, exactly as the agent would have sent them. That is your traffic
  to your provider, not data sent to the Callsheet project.
- **No other network calls** are made on your behalf unless a feature you
  enable says so here.

## What Callsheet stores (on your machine)

- **Metadata about each call by default:** model, token counts, latency,
  status, trace and span ids, and cost ([ADR 0006](docs/adr/0006-capture-modes.md)).
- **Message content only if you turn content capture on.** It is off by default.
- **Never:** API keys or auth headers. Callsheet does not log, store or echo
  them.
- **Signing keys** stay in your operating system's keychain and are used only
  by the local daemon ([ADR 0007](docs/adr/0007-identity-and-log-integrity.md)).

Everything is stored locally and stays under your control. How deletion
interacts with the tamper-evident log is being designed (issue #7) and will be
described here before it ships.

## Status

Callsheet is pre-alpha. The daemon, proxy and store that these commitments
apply to are still being built (features 02–05); this document states the
rules they are built to.
