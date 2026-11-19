//! Store library boundary for persisted Apron state and the in-memory backend.
//! Expose shared model and transaction types through this entry point.
//!
//! Proposed: async fn open_turso(path: &Path) -> Result<TursoStore, StoreError>
//! Proposed: fn open_memory() -> MemoryStore

pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
