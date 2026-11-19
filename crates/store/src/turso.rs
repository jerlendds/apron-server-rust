//! Implement the durable store with the existing Turso dependency choice. Open a
//! local database, configure transaction behavior, and run versioned migrations.
//!
//! Proposed: async fn open(path: &Path) -> Result<TursoStore, StoreError>
//! Proposed: async fn verify_schema(&self) -> Result<(), StoreError>
