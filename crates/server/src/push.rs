//! Manage push registrations, project unread and wake scopes, and deliver Web Push
//! and relay jobs outside the coordinator. Recheck visibility and endpoint policy before
//! sending and handle expired registrations.
//!
//! Proposed: async fn register(ctx: &mut RequestContext<'_>, params: PushRegistrationParams)
//!     -> Result<(), ProtocolError>
//! Proposed: async fn unregister(ctx: &mut RequestContext<'_>, url: Url)
//!     -> Result<(), ProtocolError>
//! Proposed: async fn run(worker: PushWorker, shutdown: CancellationToken)
