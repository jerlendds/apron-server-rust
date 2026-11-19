//! Handle room_list, room_join, room_leave, and room_set. Resolve default room,
//! private room visibility, immutable thread parent, and membership changes for
//! every connection of the affected user.
//!
//! Proposed: async fn list(ctx: &mut RequestContext<'_>, params: RoomListParams)
//!     -> Result<RoomListResult, ProtocolError>
//! Proposed: async fn change_membership(ctx: &mut RequestContext<'_>,
//!     params: MembershipParams, joined: bool) -> Result<(), ProtocolError>
//! Proposed: async fn set(ctx: &mut RequestContext<'_>, params: RoomSetParams)
//!     -> Result<RoomSetResult, ProtocolError>
