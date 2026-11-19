//! Compose the Axum router for /ws and enabled embed and browser sign-in endpoints.
//! Apply route-specific limits, origin policy, and logging that excludes credentials.
//!
//! Proposed: fn router(state: Arc<Service>) -> axum::Router
