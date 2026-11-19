//! Handle external sign-in begin, browser flow, and code redemption. Validate client,
//! redirect, state, expiry, and PKCE bindings before consuming an authorization code.
//!
//! Proposed: async fn begin(ctx: &AuthContext<'_>, params: ExternalBegin)
//!     -> Result<AuthChallenge, ProtocolError>
//! Proposed: async fn redeem(ctx: &AuthContext<'_>, params: ExternalRedemption)
//!     -> Result<PreparedAuth, ProtocolError>
