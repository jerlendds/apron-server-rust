//! Handle me and profile updates. Keep current user projections separate from the
//! recorded authors held in snapshots, and target user notifications by visibility.
//!
//! Proposed: async fn update_profile(ctx: &mut RequestContext<'_>,
//!     params: MeParams) -> Result<User, ProtocolError>
