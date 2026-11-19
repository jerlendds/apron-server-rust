//! Process entry point. Parse CLI options, initialize logging, select and open a store,
//! run migrations, start the server, and wait for shutdown.
//!
//! Proposed: async fn main() -> Result<(), StartupError>
//! Proposed: async fn run(config: Config) -> Result<(), StartupError>
