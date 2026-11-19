//! Stage bounded upload bodies, publish durable files and optional metadata after
//! validation, and expose completed file reads under the configured serving policy.
//!
//! Proposed: async fn ingest(body: Body, limits: &UploadLimits) -> Result<StoredAsset, WriteError>
//! Proposed: async fn get(state: Arc<Service>, id: AssetId) -> Response
