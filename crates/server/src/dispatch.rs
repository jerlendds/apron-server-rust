use serde_json::{Value, json};
use store::State;

use crate::{
    auth, commands, dedup, embeds, history, messages,
    policy::{DEFAULT_ROOM, visible},
    push, reactions, rooms,
    service::{ConnectionId, Coordinator},
    status, users,
    wire::{Error, Request, Result, notification, string},
};

impl Coordinator {
    pub async fn dispatch(&mut self, connection: ConnectionId, request: Request, oversize: bool) {
        if request.method == "ping" && !oversize {
            self.send(connection, json!({"method": "pong"}));
            return;
        }
        if request.method == "activity" {
            if !oversize && !self.config.core_only {
                let user = self
                    .connections
                    .get(&connection)
                    .and_then(|c| c.user.clone());
                if let Some((user, params)) = user.as_deref().zip(request.params.as_object()) {
                    let _ = self.activity(user, params).await;
                }
            }
            return;
        }
        let Some(id) = &request.id else { return };
        if !oversize && request.method == "auth" {
            if let Err(error) = self.sign_in(connection, &request, id).await {
                self.send(connection, error.frame(Some(id)));
            }
            return;
        }
        let result = if oversize {
            Err(Error::TooLarge)
        } else {
            self.request(connection, &request, id).await
        };
        let frame = match result {
            Ok(result) => json!({"id": id, "result": result}),
            Err(error) => error.frame(Some(id)),
        };
        self.send(connection, frame);
    }

    async fn request(
        &mut self,
        connection: ConnectionId,
        request: &Request,
        id: &str,
    ) -> Result<Value> {
        let user = self
            .connections
            .get(&connection)
            .and_then(|c| c.user.clone())
            .ok_or(Error::Denied)?;
        if let Some(retry) =
            dedup::replay(&self.state, &user, id, &request.method, &request.params)?
        {
            return match retry.method.as_str() {
                "me" => Ok(json!({"you": self.own_profile(self.state.users[&user].clone())})),
                "message" => {
                    let message_id = retry.result["message_id"].as_str().ok_or(Error::Internal)?;
                    let snapshot = self.state.messages.get(message_id).ok_or(Error::Invalid)?;
                    let room = snapshot["room_id"].as_str().ok_or(Error::Internal)?;
                    if !visible(&self.state, &user, room) {
                        return Err(Error::Invalid);
                    }
                    Ok(embeds::retry_result(&self.state, retry.result.clone()))
                }
                "room_set" => {
                    let room = retry.result["room_id"].as_str().ok_or(Error::Internal)?;
                    if !visible(&self.state, &user, room) {
                        return Err(Error::Invalid);
                    }
                    Ok(retry.result.clone())
                }
                "command" => Ok(embeds::retry_result(&self.state, retry.result.clone())),
                "room_list" => self.room_listing(&user, request),
                "history" => history::query(
                    &self.state,
                    &user,
                    request.params.as_object().ok_or(Error::Invalid)?,
                ),
                _ => Ok(retry.result.clone()),
            };
        }
        let ext_limit = self.config.ext_limit.filter(|_| !self.config.core_only);
        match request.method.as_str() {
            "me" => {
                let params = request.params.as_object().ok_or(Error::Invalid)?;
                let mut proposed = self.state.clone();
                let (mut result, changed) = users::me(
                    &mut proposed,
                    &user,
                    params,
                    ext_limit,
                    !self.config.core_only,
                )?;
                result["you"] = self.own_profile(result["you"].clone());
                self.remember_and_commit(proposed, &user, id, request, &result)
                    .await?;
                if let Some(update) = changed {
                    self.user_event(&user, &update);
                }
                Ok(result)
            }
            "message" => {
                let params = request.params.as_object().ok_or(Error::Invalid)?;
                let mut proposed = self.state.clone();
                let write =
                    messages::write(&mut proposed, &user, params, &self.config, &self.origin)?;
                let moved_reactions = if write.rooms.len() > 1 {
                    let destination = write.snapshot["room_id"].as_str().ok_or(Error::Internal)?;
                    reactions::moved(&mut proposed, &write.id, destination)?
                } else {
                    None
                };
                let mut result = json!({"message_id": write.id});
                if !write.grants.is_empty() {
                    result["embeds"] = json!(write.grants);
                }
                self.remember_and_commit(proposed, &user, id, request, &result)
                    .await?;
                self.room_event("message", &write.rooms, &write.snapshot);
                if let Some(record) = moved_reactions {
                    self.room_event("reactions", &write.rooms[..1], &record);
                }
                Ok(result)
            }
            "push_register" | "push_unregister" if self.push.is_some() => {
                let params = request.params.as_object().ok_or(Error::Invalid)?;
                let push = self.push.as_ref().ok_or(Error::Unsupported)?;
                let mut proposed = self.state.clone();
                let registrations = proposed.push_registrations.entry(user.clone()).or_default();
                if request.method == "push_register" {
                    let registration = push.registration(params)?;
                    if registrations.len() >= 32 && !registrations.contains_key(&registration.url) {
                        return Err(Error::Denied);
                    }
                    registrations.insert(registration.url.clone(), registration);
                } else {
                    let url = push::checked_url(string(params, "url")?.ok_or(Error::Invalid)?)?;
                    registrations.remove(url.as_str());
                }
                let result = json!({});
                self.remember_and_commit(proposed, &user, id, request, &result)
                    .await?;
                Ok(result)
            }
            "room_list" | "history" if !self.config.core_only => {
                let result = if request.method == "room_list" {
                    let result = self.room_listing(&user, request)?;
                    self.replay_cursors(connection, &user, &result);
                    result
                } else {
                    history::query(
                        &self.state,
                        &user,
                        request.params.as_object().ok_or(Error::Invalid)?,
                    )?
                };
                dedup::remember(
                    &mut self.state,
                    &user,
                    id.to_owned(),
                    request.method.clone(),
                    request.params.clone(),
                    json!({}),
                );
                Ok(result)
            }
            "command" if !self.config.core_only => {
                let params = request.params.as_object().ok_or(Error::Invalid)?;
                let mut proposed = self.state.clone();
                let (result, notice) = commands::run(
                    &mut proposed,
                    &user,
                    params,
                    &self.origin,
                    self.config.grant_ttl_ms,
                )?;
                self.remember_and_commit(proposed, &user, id, request, &result)
                    .await?;
                if let Some(notice) = notice {
                    self.send(connection, notification("message", notice));
                }
                Ok(result)
            }
            "status" if !self.config.core_only => {
                let params = request.params.as_object().ok_or(Error::Invalid)?;
                let mut proposed = self.state.clone();
                let change = status::apply(&mut proposed, &user, params)?;
                let result = json!({});
                self.remember_and_commit(proposed, &user, id, request, &result)
                    .await?;
                if let Some(idle) = change.idle {
                    let before = self.shown(&user);
                    self.connections
                        .get_mut(&connection)
                        .ok_or(Error::Internal)?
                        .idle = idle;
                    self.presence_change(&user, before);
                }
                if let Some(mute) = change.mute {
                    self.own_event(&user, mute);
                }
                Ok(result)
            }
            "room_set" | "room_join" | "room_leave" if !self.config.core_only => {
                let params = request.params.as_object().ok_or(Error::Invalid)?;
                let mut proposed = self.state.clone();
                let (changes, result) = if request.method == "room_set" {
                    let change = rooms::set(&mut proposed, &user, params, ext_limit)?;
                    let result = json!({"room_id": change.room});
                    (vec![change], result)
                } else {
                    (
                        rooms::membership(
                            &mut proposed,
                            &user,
                            params,
                            request.method == "room_join",
                        )?,
                        json!({}),
                    )
                };
                self.remember_and_commit(proposed, &user, id, request, &result)
                    .await?;
                for change in &changes {
                    self.room_change(change);
                }
                Ok(result)
            }
            "reactions" if !self.config.core_only => {
                let params = request.params.as_object().ok_or(Error::Invalid)?;
                let mut proposed = self.state.clone();
                let record = reactions::set(&mut proposed, &user, params)?;
                let room = record["room_id"]
                    .as_str()
                    .ok_or(Error::Internal)?
                    .to_owned();
                let result = json!({});
                self.remember_and_commit(proposed, &user, id, request, &result)
                    .await?;
                self.room_event("reactions", &[room], &record);
                Ok(result)
            }
            _ => Err(Error::Unsupported),
        }
    }

    fn room_listing(&self, user: &str, request: &Request) -> Result<Value> {
        let params = request.params.as_object().ok_or(Error::Invalid)?;
        let mut result = rooms::list(&self.state, user, params)?;
        if let Some(users) = result.get_mut("users").and_then(Value::as_array_mut) {
            for profile in users {
                let id = profile["user_id"].as_str().ok_or(Error::Internal)?;
                *profile = self.observer_profile(id, profile.clone());
            }
        }
        Ok(result)
    }

    async fn sign_in(
        &mut self,
        connection: ConnectionId,
        request: &Request,
        id: &str,
    ) -> Result<()> {
        let params = request.params.as_object().ok_or(Error::Invalid)?;
        let scheme = string(params, "scheme")?.ok_or(Error::Invalid)?;
        let token = if matches!(scheme, "email" | "token") {
            string(params, "token")?
        } else {
            None
        };
        if scheme == "email" && token.is_none() {
            self.propose_email(connection, params).await?;
            self.send(connection, json!({"id": id, "result": {}}));
            return Ok(());
        }
        let mut proposed = self.state.clone();
        let session = self.connections.get(&connection).ok_or(Error::Internal)?;
        let resume_or_guest = if scheme == "webauthn" {
            match self.passkey(&mut proposed, connection, params)? {
                auth::webauthn::Outcome::Pending(result) => {
                    self.send(connection, json!({"id": id, "result": result}));
                    return Ok(());
                }
                auth::webauthn::Outcome::SignedIn(user, result, created) => {
                    Ok((user, result, created))
                }
            }
        } else if scheme == "external" && self.config.external && !self.config.core_only {
            if !params.contains_key("code") {
                let result = auth::external::begin(&self.origin, params)?;
                self.send(connection, json!({"id": id, "result": result}));
                return Ok(());
            }
            auth::external::redeem(
                &mut proposed,
                params,
                session.user.as_deref(),
                &self.origin,
                "external",
            )
        } else {
            auth::auth(&mut proposed, params, session.origin.as_deref())
        };
        let (user, mut result, created, changed) = match resume_or_guest {
            Ok((user, result, created)) => (
                user,
                result,
                created,
                created || matches!(scheme, "external" | "webauthn"),
            ),
            Err(_) if self.mail.is_some() && token.is_some() => {
                let session = self.connections.get(&connection).ok_or(Error::Internal)?;
                let approval = auth::email::approve(
                    &mut proposed,
                    params,
                    session.user.as_deref(),
                    session.pending_email.as_deref(),
                );
                match approval.outcome {
                    Ok((user, result, created)) => (user, result, created, true),
                    Err(error) => {
                        if approval.changed {
                            self.commit(proposed).await?;
                        }
                        return Err(error);
                    }
                }
            }
            Err(error) => return Err(error),
        };
        result["you"] = self.own_profile(result["you"].clone());
        let previous = self
            .connections
            .get(&connection)
            .and_then(|c| c.user.clone());
        let previous_presence = previous.as_deref().map(|id| self.shown(id));
        let before = if created {
            "offline"
        } else {
            self.shown(&user)
        };
        let mut membership = None;
        if created {
            if !self.config.core_only {
                membership = rooms::change_membership(&mut proposed, DEFAULT_ROOM, &user, true)?;
            } else {
                proposed
                    .members
                    .entry(DEFAULT_ROOM.into())
                    .or_default()
                    .insert(user.clone());
            }
        }
        if changed {
            self.commit(proposed).await?;
        }
        let session = self
            .connections
            .get_mut(&connection)
            .ok_or(Error::Internal)?;
        session.user = Some(user.clone());
        self.send(connection, json!({"id": id, "result": result}));
        if previous.as_deref() != Some(&user) {
            self.status_at_sign_in(connection, &user);
            self.presence_change(&user, before);
            if let Some((previous, presence)) = previous.as_deref().zip(previous_presence) {
                self.presence_change(previous, presence);
            }
        }
        if let Some(membership) = membership {
            self.room_change(&rooms::Change {
                room: DEFAULT_ROOM.to_owned(),
                record_changed: false,
                membership: Some(membership),
            });
        }
        Ok(())
    }

    async fn remember_and_commit(
        &mut self,
        mut proposed: State,
        user: &str,
        id: &str,
        request: &Request,
        result: &Value,
    ) -> Result<()> {
        dedup::remember(
            &mut proposed,
            user,
            id.to_owned(),
            request.method.clone(),
            request.params.clone(),
            result.clone(),
        );
        let message = (request.method == "message")
            .then(|| result["message_id"].as_str())
            .flatten()
            .filter(|id| !self.state.messages.contains_key(*id))
            .map(str::to_owned);
        self.commit_state(proposed, message).await
    }

    pub(crate) async fn commit(&mut self, proposed: State) -> Result<()> {
        self.commit_state(proposed, None).await
    }

    async fn commit_state(&mut self, proposed: State, message: Option<String>) -> Result<()> {
        let badges = if self.push_queue.is_some() {
            push::badge_changes(&self.state, &proposed)
        } else {
            Default::default()
        };
        self.store
            .commit(&proposed)
            .await
            .map_err(|_| Error::Internal)?;
        self.state = proposed;
        if let Some(id) = message {
            self.push_signal(push::Signal::Message { id, badges });
        } else {
            for user in badges {
                self.push_signal(push::Signal::Badge(user));
            }
        }
        self.live.retain(|key, live| {
            let retained = self.state.messages.values().any(|message| {
                message["body"]["embeds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|embed| {
                        embed["url"]
                            .as_str()
                            .and_then(|url| url.strip_prefix(&self.origin))
                            .and_then(|path| path.strip_prefix("/live/"))
                            == Some(key.as_str())
                    })
            });
            if !retained {
                live.cancelled.cancel();
            }
            retained
        });
        Ok(())
    }
}
