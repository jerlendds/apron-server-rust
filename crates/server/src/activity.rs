use serde_json::{Value, json};

use crate::{
    policy::{DEFAULT_ROOM, joined, visible},
    service::{ConnectionId, Coordinator},
    wire::{Error, Params, Result, notification, string},
};

impl Coordinator {
    pub async fn activity(&mut self, actor: &str, params: &Params) -> Result<()> {
        let room = string(params, "room_id")?.unwrap_or(DEFAULT_ROOM);
        if !visible(&self.state, actor, room) || !joined(&self.state, actor, room) {
            return Err(Error::Invalid);
        }
        let mut activity = json!({"room_id": room, "from": {"user_id": actor}});
        if let Some(typing) = params.get("typing") {
            activity["typing"] = json!(typing.as_u64().ok_or(Error::Invalid)?.min(60));
        }
        if let Some(message) = string(params, "read_message_id")? {
            let snapshot = self.state.messages.get(message).ok_or(Error::Invalid)?;
            if snapshot["room_id"] != room {
                return Err(Error::Invalid);
            }
            let position = message.parse::<u64>().map_err(|_| Error::Internal)?;
            let last = self
                .state
                .read_cursors
                .get(actor)
                .and_then(|rooms| rooms.get(room))
                .copied()
                .unwrap_or(0);
            if last < position {
                let mut proposed = self.state.clone();
                proposed
                    .read_cursors
                    .entry(actor.to_owned())
                    .or_default()
                    .insert(room.to_owned(), position);
                self.commit(proposed).await?;
                activity["read_message_id"] = json!(message);
            }
        }
        if activity.get("typing").is_some() || activity.get("read_message_id").is_some() {
            self.room_event("activity", &[room.to_owned()], &activity);
        }
        Ok(())
    }
}

impl Coordinator {
    pub fn replay_cursors(&self, connection: ConnectionId, user: &str, listing: &Value) {
        let Some(cursors) = self.state.read_cursors.get(user) else {
            return;
        };
        for room in listing["joined"].as_array().into_iter().flatten() {
            let Some(room) = room["room_id"].as_str() else {
                continue;
            };
            if let Some(cursor) = cursors.get(room) {
                self.send(connection, notification("activity", json!({
                    "room_id": room, "from": {"user_id": user}, "read_message_id": cursor.to_string(),
                })));
            }
        }
    }
}
