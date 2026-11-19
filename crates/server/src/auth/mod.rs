//! Select among advertised schemes and perform every auth request on its connection.
//! Prepare provider work and pass verified outcomes to the coordinator for sign-in.
//!
//! Proposed: async fn prepare(state: &AuthState, params: AuthParams)
//!     -> Result<PreparedAuth, ProtocolError>
//! Proposed: async fn finish(ctx: &mut AuthContext<'_>, prepared: PreparedAuth)
//!     -> Result<AuthOutcome, ProtocolError>
