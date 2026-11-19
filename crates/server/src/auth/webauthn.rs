//! Manage passkey challenges, registration and assertion verification, and the browser
//! passkey page flow. Bind one-time state to its intended origin and relying party.
//!
//! Proposed: async fn begin(ctx: &AuthContext<'_>, params: WebAuthnBegin)
//!     -> Result<AuthChallenge, ProtocolError>
//! Proposed: async fn finish(ctx: &AuthContext<'_>, params: WebAuthnFinish)
//!     -> Result<PreparedAuth, ProtocolError>
