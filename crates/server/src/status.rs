//! Calculate user presence from live connections and persisted user choices. Handle
//! status requests, idle transitions, mute scopes, and user-specific notifications.
//!
//! Proposed: async fn update(ctx: &mut RequestContext<'_>, params: StatusParams)
//!     -> Result<StatusResult, ProtocolError>
//! Proposed: fn shown_status(connections: &[ConnectionPresence], chosen: &ChosenStatus)
//!     -> ShownStatus
