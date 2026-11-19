//! Accept scoped HTTP PUTs, claim a grant once, and finish or expire it through the
//! coordinator. Use the current owner record when applying a write's outcome.
//!
//! Proposed: async fn put(state: Arc<Service>, grant: WriteToken, body: Body) -> Response
//! Proposed: async fn finish(ctx: &mut MutationContext<'_>, claim: WriteClaim,
//!     outcome: WriteOutcome) -> Result<(), ProtocolError>
