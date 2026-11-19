//! Project committed records into recipient-specific frames. Track memberships before and
//! after changes, send each record once per connection, and enqueue notifications before
//! their initiating reply except at sign-in. Disconnect stalled writers.
//!
//! Proposed: fn project(change: &CommittedChange, audience: &Audience) -> Deliveries
//! Proposed: fn enqueue(&mut self, batch: OrderedBatch) -> Vec<ConnectionId>
