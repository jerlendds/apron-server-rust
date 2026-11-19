//! Track connection identity, subscriptions, greeting, and liveness. Apply an auth
//! transition to the connection and enqueue its result before sign-in notifications.
//!
//! Proposed: fn greeting(config: &ServerConfig, identity: Option<&Principal>) -> ServerFrame
//! Proposed: fn apply_auth(&mut self, outcome: AuthOutcome) -> OrderedBatch
