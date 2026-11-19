//! Describe future versioned Turso schema migrations for log state, projections,
//! room indexes, one-time credentials, grants, and retry results. Check the schema
//! version before accepting traffic.
//!
//! Proposed: async fn migrate(database: &Database) -> Result<(), StoreError>
