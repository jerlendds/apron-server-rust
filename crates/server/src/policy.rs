//! Centralize room visibility, editing, command, role, and feature policy. Apply the same
//! visibility predicates to live frames, history, references, and push projections.
//!
//! Proposed: fn authorize(actor: &Principal, action: &PolicyAction,
//!     facts: &AccessFacts) -> Result<(), ProtocolError>
