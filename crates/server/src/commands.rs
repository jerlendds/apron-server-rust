//! Parse slash commands in the server's command registry and authorize each handler.
//! Use the same mutations as ordinary requests, with scoped notices or private write
//! grants as appropriate. The command itself never becomes a log record.
//!
//! Proposed: async fn execute(ctx: &mut RequestContext<'_>, params: CommandParams)
//!     -> Result<CommandResult, ProtocolError>
