//! Library entry point for the Apron server. Build the shared state and HTTP routes
//! around a caller-supplied store and run until the daemon signals shutdown.
//!
//! Proposed: async fn serve(listener: TcpListener, config: ServerConfig, store: Arc<dyn Store>,
//!     shutdown: CancellationToken) -> Result<(), ServeError>
