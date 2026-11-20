use serde_json::{Value, json};
use store::{State, epoch_millis};

use crate::{
    policy::{shares_room, visible},
    service::{ConnectionId, Coordinator},
    wire::{Error, Params, Result, notification, string},
};

pub(crate) const OPTIONAL: &[&str] = &["dnd", "invisible"];

pub(crate) struct Change {
    pub idle: Option<bool>,
    pub mute: Option<Value>,
}

pub(crate) fn apply(state: &mut State, user: &str, params: &Params) -> Result<Change> {
    let idle = params
        .get("idle")
        .map(|value| match value {
            Value::Bool(false) => Ok(false),
            value if value.as_u64().is_some() => Ok(true),
            _ => Err(Error::Invalid),
        })
        .transpose()?;
    let room = string(params, "room_id")?;
    // room_id only scopes a mute (§4.5.3); on its own it changes nothing.
    if room.is_some() && !params.contains_key("mute") {
        return Err(Error::Invalid);
    }
    if room.is_some_and(|room| !visible(state, user, room)) {
        return Err(Error::Invalid);
    }
    let mute = params
        .get("mute")
        .map(|value| {
            let expires = match value {
                Value::Bool(true) => u64::MAX,
                Value::Bool(false) => 0,
                value => {
                    let seconds = value
                        .as_u64()
                        .filter(|seconds| *seconds > 0)
                        .ok_or(Error::Invalid)?;
                    epoch_millis()
                        .checked_add(seconds.checked_mul(1000).ok_or(Error::Invalid)?)
                        .filter(|expires| *expires < u64::MAX)
                        .ok_or(Error::Invalid)?
                }
            };
            let scopes = state.mutes.entry(user.to_owned()).or_default();
            let scope = room.unwrap_or_default().to_owned();
            if expires == 0 {
                scopes.remove(&scope);
            } else {
                scopes.insert(scope, expires);
            }
            Ok(mute_frame(room, value.clone()))
        })
        .transpose()?;
    Ok(Change { idle, mute })
}

fn mute_frame(room: Option<&str>, mute: Value) -> Value {
    let mut params = json!({"mute": mute});
    if let Some(room) = room {
        params["room_id"] = json!(room);
    }
    notification("status", params)
}

fn mute_value(expires: u64, now: u64) -> Option<Value> {
    match expires {
        u64::MAX => Some(json!(true)),
        expires if expires > now => Some(json!((expires - now).div_ceil(1000))),
        _ => None,
    }
}

pub(crate) fn chosen(profile: &Value) -> &str {
    profile["status"].as_str().unwrap_or("online")
}

/// The server's view of a user's connections (§4.5.1).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Presence {
    Online,
    Idle,
    Offline,
}

/// The status others see, from the chosen status and the presence (§4.5.1).
pub(crate) fn shown(chosen: &str, presence: Presence) -> &'static str {
    match (chosen, presence) {
        ("", _) => "",
        ("invisible", _) | (_, Presence::Offline) => "offline",
        ("dnd", _) => "dnd",
        (_, Presence::Online) => "online",
        (_, Presence::Idle) => "idle",
    }
}

impl Coordinator {
    pub fn own_profile(&self, mut profile: Value) -> Value {
        if !self.config.core_only && profile.get("status").is_none() {
            profile["status"] = json!("online");
        }
        profile
    }

    pub fn shown(&self, user: &str) -> &'static str {
        let mut sessions = self
            .connections
            .values()
            .filter(|connection| connection.user.as_deref() == Some(user))
            .peekable();
        let presence = if sessions.peek().is_none() {
            Presence::Offline
        } else if sessions.any(|connection| !connection.idle) {
            Presence::Online
        } else {
            Presence::Idle
        };
        shown(chosen(&self.state.users[user]), presence)
    }

    pub fn observer_profile(&self, user: &str, mut profile: Value) -> Value {
        if !self.config.core_only {
            profile["status"] = json!(self.shown(user));
        }
        profile
    }

    pub fn presence_change(&self, user: &str, previous: &'static str) {
        if self.config.core_only || previous == self.shown(user) {
            return;
        }
        let update = notification(
            "user",
            json!({
                "new": {"user_id": user, "status": self.shown(user)}
            }),
        );
        for (&id, connection) in &self.connections {
            if let Some(member) = &connection.user
                && member != user
                && shares_room(&self.state, member, user)
            {
                self.send(id, update.clone());
            }
        }
    }

    pub fn status_at_sign_in(&self, connection: ConnectionId, user: &str) {
        if self.config.core_only {
            return;
        }
        for (other, profile) in &self.state.users {
            if other == user || !shares_room(&self.state, other, user) {
                continue;
            }
            if matches!(self.shown(other), "offline" | "") {
                continue;
            }
            self.send(
                connection,
                notification(
                    "user",
                    json!({"new": self.observer_profile(other, profile.clone())}),
                ),
            );
        }
        let now = epoch_millis();
        for (scope, &expires) in self.state.mutes.get(user).into_iter().flatten() {
            if let Some(mute) = mute_value(expires, now)
                && (scope.is_empty() || visible(&self.state, user, scope))
            {
                self.send(
                    connection,
                    mute_frame((!scope.is_empty()).then_some(scope), mute),
                );
            }
        }
    }

    pub fn own_event(&self, user: &str, frame: Value) {
        for (&id, connection) in &self.connections {
            if connection.user.as_deref() == Some(user) {
                self.send(id, frame.clone());
            }
        }
    }

    pub async fn expire_mutes(&mut self) {
        let now = epoch_millis();
        let expired = self
            .state
            .mutes
            .iter()
            .flat_map(|(user, scopes)| {
                scopes
                    .iter()
                    .filter(move |(_, expires)| **expires <= now)
                    .map(move |(scope, _)| (user.clone(), scope.clone()))
            })
            .collect::<Vec<_>>();
        if expired.is_empty() {
            return;
        }
        let mut proposed = self.state.clone();
        for (user, scope) in &expired {
            if let Some(scopes) = proposed.mutes.get_mut(user) {
                scopes.remove(scope);
            }
        }
        if self.commit(proposed).await.is_ok() {
            for (user, scope) in expired {
                if scope.is_empty() || visible(&self.state, &user, &scope) {
                    self.own_event(
                        &user,
                        mute_frame((!scope.is_empty()).then_some(scope.as_str()), json!(false)),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! §4.5.1: every cell of the chosen status × presence table.
    use super::{Presence, chosen, shown};
    use serde_json::json;

    #[test]
    fn shown_status_matches_every_cell_of_the_spec_table() {
        use Presence::{Idle, Offline, Online};
        let table = [
            ("online", ["online", "idle", "offline"]),
            ("dnd", ["dnd", "dnd", "offline"]),
            ("invisible", ["offline", "offline", "offline"]),
            ("", ["", "", ""]),
        ];
        for (choice, row) in table {
            for (presence, expected) in [Online, Idle, Offline].into_iter().zip(row) {
                assert_eq!(
                    shown(choice, presence),
                    expected,
                    "{choice:?} × {presence:?}"
                );
            }
        }
    }

    #[test]
    fn an_unset_chosen_status_defaults_to_online() {
        assert_eq!(chosen(&json!({"user_id": "ada"})), "online");
        assert_eq!(chosen(&json!({"user_id": "ada", "status": ""})), "");
        assert_eq!(chosen(&json!({"user_id": "ada", "status": "dnd"})), "dnd");
    }
}
