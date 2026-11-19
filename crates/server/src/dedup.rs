//! Match authenticated retries by user and request ID, using method and canonical
//! parameters. Replay from current state and stored operation references. Exclude auth,
//! and reject an ID reused with different inputs.
//!
//! Proposed: fn fingerprint(method: &str, params: &JsonValue) -> RequestFingerprint
//! Proposed: async fn check(tx: &mut dyn WriteTx, key: &RequestKey,
//!     fingerprint: &RequestFingerprint) -> Result<RetryDecision, ProtocolError>
