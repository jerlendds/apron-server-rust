//! Create, edit, move, and tombstone complete snapshots. Resolve reply references and
//! explicit mentions, preserve recorded authors, and index moves in both rooms.
//!
//! Proposed: async fn write(ctx: &mut RequestContext<'_>, params: MessageParams)
//!     -> Result<MessageResult, ProtocolError>
//! Proposed: async fn resolve_reply(view: &dyn ReadTx, actor: &Principal, target: MessageId)
//!     -> Result<ReplyRef, ProtocolError>
