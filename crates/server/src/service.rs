//! Coordinate authoritative reads, mutations, connection changes, and outbound batches.
//! Serialize state transitions with commit and enqueue. Recheck prepared work here
//! after any off-coordinator provider I/O.
//!
//! Proposed: async fn submit(&self, action: Action) -> Result<(), ServiceError>
//! Proposed: async fn run(self, actions: Receiver<Action>) -> Result<(), ServiceError>
