//! Stream append text and terminal output over HTTP with bounded buffering.
//! Finalize append content into the message body and terminal output into the embed's
//! output field. Send snapshot changes through the same mutation path as edits.
//!
//! Proposed: async fn ingest(kind: StreamKind, body: Body, sink: StreamSink)
//!     -> Result<StreamResult, WriteError>
//! Proposed: async fn get(state: Arc<Service>, id: StreamId) -> Response
