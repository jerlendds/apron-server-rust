//! Handle email proposals, short-lived sign-in tokens, and approval. Bound delivery
//! and retry rates and commit credential consumption together with account changes.
//!
//! Proposed: async fn propose(ctx: &AuthContext<'_>, params: EmailProposal)
//!     -> Result<PreparedAuth, ProtocolError>
//! Proposed: async fn confirm(ctx: &AuthContext<'_>, params: EmailConfirmation)
//!     -> Result<PreparedAuth, ProtocolError>
