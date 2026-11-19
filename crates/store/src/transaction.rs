//! Define object-safe async store and transaction interfaces with a consistent read
//! snapshot and an atomic write. Include operations for projections, log appends,
//! retry records, and one-time state transitions.
//!
//! Proposed: async fn begin_read(&self) -> Result<Box<dyn ReadTx + '_>, StoreError>
//! Proposed: async fn begin_write(&self) -> Result<Box<dyn WriteTx + '_>, StoreError>
//! Proposed: async fn commit(self: Box<Self>) -> Result<(), StoreError>
