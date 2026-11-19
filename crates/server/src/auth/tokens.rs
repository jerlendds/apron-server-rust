//! Handle guest identities, bearer credentials, token rotation, expiry, and revocation.
//! Atomically look up or consume credentials and keep raw tokens out of storage and logs.
//!
//! Proposed: async fn guest(tx: &mut dyn WriteTx, params: GuestParams)
//!     -> Result<AuthOutcome, ProtocolError>
//! Proposed: async fn redeem(tx: &mut dyn WriteTx, params: TokenParams)
//!     -> Result<AuthOutcome, ProtocolError>
