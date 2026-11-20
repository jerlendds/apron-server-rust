use serde_json::{Value, json};

use crate::{
    policy::{joined, shares_room, visible},
    rooms,
    service::{ConnectionId, Coordinator},
    wire::notification,
};

impl Coordinator {
    pub fn send(&self, id: ConnectionId, frame: Value) {
        if let Some(connection) = self.connections.get(&id)
            && connection.tx.try_send(frame).is_err()
        {
            connection.closed.cancel();
        }
    }

    pub fn room_event(&self, method: &str, rooms: &[String], snapshot: &Value) {
        for (&id, connection) in &self.connections {
            let Some(user) = &connection.user else {
                continue;
            };
            if !self.config.core_only && !rooms.iter().all(|room| visible(&self.state, user, room))
            {
                continue;
            }
            if self.config.core_only || rooms.iter().any(|room| joined(&self.state, user, room)) {
                self.send(id, notification(method, snapshot.clone()));
            }
        }
    }

    pub fn room_change(&self, change: &rooms::Change) {
        let room = &change.room;
        let record = &self.state.rooms[room];
        for (&connection_id, connection) in &self.connections {
            let Some(user) = &connection.user else {
                continue;
            };
            let member = joined(&self.state, user, room) && visible(&self.state, user, room);
            let mut update = json!({});
            if let Some(membership) = &change.membership {
                let own_change = membership["members"][0]["user"]["user_id"] == user.as_str();
                if own_change || member {
                    update["memberships"] = json!([membership]);
                }
                if own_change {
                    if membership["members"][0]["joined"] == true {
                        let projected = rooms::project(&self.state, room, true, None);
                        let users = projected["members"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|member| member["user_id"].as_str())
                            .filter_map(|user| {
                                self.state
                                    .users
                                    .get(user)
                                    .map(|profile| self.observer_profile(user, profile.clone()))
                            })
                            .collect::<Vec<_>>();
                        update["users"] = json!(users);
                        update["joined"] = json!([projected]);
                    } else {
                        update["left"] = json!([{"room_id": room}]);
                    }
                }
            }
            let parent_member = record["private"] != true
                && record["parent_room_id"].as_str().is_some_and(|parent| {
                    joined(&self.state, user, parent) && visible(&self.state, user, room)
                });
            if change.record_changed && update["joined"].is_null() && (member || parent_member) {
                update["updated"] = json!([rooms::project(&self.state, room, false, None)]);
            }
            if update != json!({}) {
                self.send(connection_id, notification("room_update", update));
            }
        }
    }

    pub fn user_event(&self, user: &str, update: &Value) {
        for (&id, connection) in &self.connections {
            let Some(recipient) = &connection.user else {
                continue;
            };
            if recipient == user {
                self.send(
                    id,
                    notification("user", json!({"you": self.own_profile(update.clone())})),
                );
            } else if shares_room(&self.state, recipient, user) {
                self.send(
                    id,
                    notification(
                        "user",
                        json!({"new": self.observer_profile(user, update.clone())}),
                    ),
                );
            }
        }
    }
}
