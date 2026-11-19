//! Implement the same store contract in memory for ephemeral runs and conformance
//! tests. Preserve consistent reads, commit ordering, and rollback semantics.
//!
//! Proposed: fn new() -> MemoryStore
