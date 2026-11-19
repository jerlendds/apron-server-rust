//! Resolve authorized room or aggregate scope. Map a consistent store page to the
//! wire response and preserve inclusive bounds and pre-compaction continuation IDs.
//!
//! Proposed: async fn page(ctx: &mut RequestContext<'_>, params: HistoryParams)
//!     -> Result<HistoryResult, ProtocolError>
