//! Define directional JSON envelopes, method parameters and results, notifications,
//! and protocol errors. Decode unknown names and fields according to PROTOCOL.md §1.
//! Map JSON string identifiers to typed store IDs with explicit validation.
//!
//! Proposed: fn decode(text: &str, limits: &WireLimits) -> Result<ClientFrame, DecodeError>
//! Proposed: fn encode(frame: &ServerFrame) -> Result<String, EncodeError>
