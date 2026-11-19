//! Classify known methods by direction and frame kind, gate features, and route
//! authenticated requests and notifications. Handle unauthenticated ping and protocol
//! errors according to PROTOCOL.md §§1 and 3.2.2.
//!
//! Proposed: async fn request(ctx: &mut RequestContext<'_>, request: Request) -> RequestOutcome
//! Proposed: async fn notification(ctx: &mut RequestContext<'_>, notification: ClientNotification)
