//! Validate embed kinds and metadata, prepare pending records and private write grants,
//! and resolve existing embed IDs and avatar references through shared policy.
//!
//! Proposed: async fn prepare(ctx: &mut RequestContext<'_>, embeds: Vec<EmbedInput>)
//!     -> Result<PreparedEmbeds, ProtocolError>
//! Proposed: fn avatar_url(embed: &CompletedEmbed) -> Result<Url, ProtocolError>
