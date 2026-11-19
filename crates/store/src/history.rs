//! Read an authorized scope supplied by server as one consistent page. Select a
//! contiguous mixed-record slice before optional folding, and retain empty reaction
//! sets and leaves. Keep page bounds and retention state from the same snapshot.
//!
//! Proposed: async fn page(view: &dyn ReadTx, query: LogQuery)
//!     -> Result<LogPage, StoreError>
//! Proposed: fn compact(records: Vec<LogRecord>) -> Vec<LogRecord>
