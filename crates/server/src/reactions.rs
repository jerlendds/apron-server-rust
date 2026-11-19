//! Normalize and replace a user's full emoji set, including empty resets. Append the
//! record and resolve the audience in the message's current room.
//!
//! Proposed: async fn replace(ctx: &mut RequestContext<'_>, params: ReactionsParams)
//!     -> Result<(), ProtocolError>
