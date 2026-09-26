//! JSON-RPC 2.0 parsing, dispatch and batches (R2.1, R2.6).
//!
//! Owned by unit `rpc-http`: parses raw JSON so malformed requests get -32700 or
//! -32600; dispatches through a handler trait; batches of 1 to 16 in order, with no
//! entries for notifications, HTTP 204 for a batch of only notifications, and one
//! -32600 with `id: null` for an empty array or more than 16 entries.
