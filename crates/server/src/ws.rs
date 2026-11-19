//! Own one WebSocket's reader, ordered writer, bounded outgoing queue, and liveness.
//! Submit inbound frames in order so auth finishes before later frames are processed.
//!
//! Proposed: async fn upgrade(state: Arc<Service>, request: WebSocketUpgrade) -> Response
//! Proposed: async fn run(socket: WebSocket, service: Arc<Service>) -> Result<(), SessionError>
