//! Implement the top-level ext key merge shared by profile, room, and message writes.
//! Validate the merged size before commit and preserve opaque extension values.
//!
//! Proposed: fn merge(current: &ExtMap, patch: &ExtMap, limit: usize)
//!     -> Result<ExtMap, ProtocolError>
