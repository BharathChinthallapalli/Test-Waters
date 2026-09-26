//! The HTTP layer of the control API (R2.1, R2.3, R2.4).
//!
//! Owned by unit `rpc-http`: one route, `POST /rpc`, with layers outermost first:
//! 64 KiB body limit, 408 after 10 s, 403 for any `Origin` header or a `Host` other
//! than `127.0.0.1:<port>`, 401 for a missing or wrong bearer token, then dispatch.
//! Header values are never logged.
