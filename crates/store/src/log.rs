//! Allocate positive server-wide log IDs below 2^53, using a high-water mark persisted
//! with each write. Index a record by each affected room without cloning its identity.
//!
//! Proposed: fn next_log_id(now_ms: u64, previous: Option<LogId>) -> Result<LogId, StoreError>
//! Proposed: async fn allocate(tx: &mut dyn WriteTx, now_ms: u64) -> Result<LogId, StoreError>
//! Proposed: async fn append(tx: &mut dyn WriteTx, record: LogRecord,
//!     rooms: &[RoomId]) -> Result<(), StoreError>
