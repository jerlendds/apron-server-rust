//! Process typing and read markers as notification-only updates. Bound typing
//! lifetimes, validate room delivery scope, and optionally keep monotonic read cursors.
//!
//! Proposed: async fn apply(ctx: &mut RequestContext<'_>, params: ActivityParams)
