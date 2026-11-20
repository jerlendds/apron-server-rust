use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Certificate, Client, StatusCode};
use serde_json::{Value, json};
use store::{PushKeys, PushRegistration, State, epoch_millis};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};
use web_push::{ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushMessageBuilder};

use crate::{
    auth::random_secret,
    policy::{joined, visible},
    service::{Coordinator, Server},
    status,
    wire::{Error, Params, Result, object, string},
};

const SCOPES: &[&str] = &["mentions", "replies", "private", "joined", "badge"];
const TTL: u32 = 300;

#[derive(Clone, Debug)]
pub struct PushConfig {
    pub vapid_key: Option<PathBuf>,
    /// Explicit trust for a private push service at one origin.
    pub private_origin: Option<String>,
    pub private_ca: Option<PathBuf>,
}

pub(crate) struct Push {
    vapid: Option<Vec<u8>>,
    public_key: Option<String>,
    private: Option<(String, Certificate)>,
}

pub(crate) enum Signal {
    Message {
        id: String,
        badges: BTreeSet<String>,
    },
    Badge(String),
}

impl Push {
    pub async fn open(
        config: &PushConfig,
    ) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (vapid, public_key) = if let Some(path) = &config.vapid_key {
            let key = tokio::fs::read(path).await?;
            let public = VapidSignatureBuilder::from_pem_no_sub(key.as_slice())
                .map_err(|_| "invalid VAPID signing key")?
                .get_public_key();
            (Some(key), Some(URL_SAFE_NO_PAD.encode(public)))
        } else {
            (None, None)
        };
        let private = match (&config.private_origin, &config.private_ca) {
            (None, None) => None,
            (Some(origin), Some(ca)) => {
                let url = checked_url(origin).map_err(|_| "invalid private push origin")?;
                if url.path() != "/" || url.query().is_some() {
                    return Err("private push trust requires an HTTPS origin".into());
                }
                let cert = Certificate::from_pem(&tokio::fs::read(ca).await?)?;
                Some((url.origin().ascii_serialization(), cert))
            }
            _ => return Err("private push origin and CA must be configured together".into()),
        };
        Ok(Self {
            vapid,
            public_key,
            private,
        })
    }

    pub fn greeting(&self) -> Value {
        let mut result = json!({"relay": {}, "wake": SCOPES});
        if let Some(key) = &self.public_key {
            result["webpush"] = json!({"key": key});
        }
        result
    }

    pub fn registration(&self, params: &Params) -> Result<PushRegistration> {
        let kind = string(params, "kind")?.ok_or(Error::Invalid)?;
        if kind != "relay" && !(kind == "webpush" && self.vapid.is_some()) {
            return Err(Error::Invalid);
        }
        let url = checked_url(string(params, "url")?.ok_or(Error::Invalid)?)?;
        let token = if kind == "relay" {
            string(params, "token")?
        } else {
            None
        };
        if token.is_some_and(|token| {
            token.is_empty()
                || token.len() > 4096
                || !token.bytes().all(|byte| byte.is_ascii_graphic())
        }) {
            return Err(Error::Invalid);
        }
        let push_id = string(params, "push_id")?;
        if push_id.is_some_and(|id| {
            id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        }) {
            return Err(Error::Invalid);
        }
        let wake = if let Some(wake) = params.get("wake") {
            wake.as_array()
                .ok_or(Error::Invalid)?
                .iter()
                .map(|name| name.as_str().map(str::to_owned).ok_or(Error::Invalid))
                .collect::<Result<BTreeSet<_>>>()?
        } else {
            ["mentions".into(), "replies".into()].into()
        };
        let wake = wake
            .into_iter()
            .filter(|scope| {
                SCOPES.contains(&scope.as_str()) && (kind != "webpush" || scope != "badge")
            })
            .collect();
        let keys = object(params, "keys")?
            .map(|keys| {
                let p256dh = string(keys, "p256dh")?.ok_or(Error::Invalid)?;
                let auth = string(keys, "auth")?.ok_or(Error::Invalid)?;
                let point = URL_SAFE_NO_PAD.decode(p256dh).map_err(|_| Error::Invalid)?;
                if point.len() != 65
                    || point[0] != 4
                    || URL_SAFE_NO_PAD
                        .decode(auth)
                        .map_or(true, |auth| auth.len() != 16)
                {
                    return Err(Error::Invalid);
                }
                let subscription = SubscriptionInfo::new(url.as_str(), p256dh, auth);
                let mut validator = WebPushMessageBuilder::new(&subscription);
                validator.set_payload(ContentEncoding::Aes128Gcm, b"{}");
                validator.build().map_err(|_| Error::Invalid)?;
                Ok(PushKeys {
                    p256dh: p256dh.to_owned(),
                    auth: auth.to_owned(),
                })
            })
            .transpose()?;
        if kind == "webpush" && keys.is_none() {
            return Err(Error::Invalid);
        }
        Ok(PushRegistration {
            url: url.into(),
            kind: kind.to_owned(),
            token: token.map(str::to_owned),
            push_id: push_id.map(str::to_owned),
            keys,
            wake,
            version: random_secret()?,
        })
    }

    pub async fn validate_endpoint(
        &self,
        url: &Url,
    ) -> Result<(Vec<SocketAddr>, Option<Certificate>)> {
        let host = url
            .host_str()
            .ok_or(Error::Invalid)?
            .trim_matches(['[', ']']);
        let port = url.port_or_known_default().ok_or(Error::Invalid)?;
        let local_ca = self
            .private
            .as_ref()
            .filter(|(trusted, _)| *trusted == url.origin().ascii_serialization())
            .map(|(_, ca)| ca.clone());
        let addresses = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| Error::Invalid)?
        .map_err(|_| Error::Invalid)?
        .collect::<Vec<_>>();
        if addresses.is_empty()
            || (local_ca.is_none() && addresses.iter().any(|address| !public_ip(address.ip())))
        {
            return Err(Error::Invalid);
        }
        Ok((addresses, local_ca))
    }

    async fn send(&self, delivery: &Delivery) -> std::result::Result<StatusCode, ()> {
        let registration = &delivery.registration;
        let url = checked_url(&registration.url).map_err(|_| ())?;
        let (addresses, ca) = self.validate_endpoint(&url).await.map_err(|_| ())?;
        let audience = url.origin().ascii_serialization();
        let mut client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5));
        if let Some(Host::Domain(host)) = url.host() {
            client = client.resolve_to_addrs(host, &addresses);
        }
        if let Some(ca) = ca {
            client = client
                .tls_built_in_root_certs(false)
                .add_root_certificate(ca);
        }
        let client = client.build().map_err(|_| ())?;
        let mut request = client
            .post(url)
            .header("TTL", TTL)
            .header("Urgency", delivery.urgency);
        if let Some(token) = &registration.token {
            request = request.bearer_auth(token);
        }
        if let Some(keys) = &registration.keys {
            let subscription = SubscriptionInfo::new(&registration.url, &keys.p256dh, &keys.auth);
            let mut message = WebPushMessageBuilder::new(&subscription);
            if registration.kind == "webpush" {
                let vapid = self.vapid.as_deref().ok_or(())?;
                let mut signature =
                    VapidSignatureBuilder::from_pem(vapid, &subscription).map_err(|_| ())?;
                signature.add_claim("aud", audience);
                let signature = signature.build().map_err(|_| ())?;
                message.set_vapid_signature(signature);
            }
            message.set_payload(ContentEncoding::Aes128Gcm, &delivery.payload);
            let payload = message.build().map_err(|_| ())?.payload.ok_or(())?;
            request = request
                .header("Content-Type", "application/octet-stream")
                .header("Content-Encoding", "aes128gcm");
            for (name, value) in payload.crypto_headers {
                request = request.header(name, value);
            }
            request = request.body(payload.content);
        } else {
            request = request
                .header("Content-Type", "application/json")
                .body(delivery.payload.clone());
        }
        request
            .send()
            .await
            .map(|response| response.status())
            .map_err(|_| ())
    }
}

pub(crate) fn checked_url(value: &str) -> Result<Url> {
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    let url = Url::parse(value).map_err(|_| Error::Invalid)?;
    if url.scheme() != "https"
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Invalid);
    }
    Ok(url)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !matches!(
                (a, b, c),
                (0 | 10 | 127, _, _)
                    | (100, 64..=127, _)
                    | (169, 254, _)
                    | (172, 16..=31, _)
                    | (192, 0, 0 | 2)
                    | (192, 168, _)
                    | (192, 88, 99)
                    | (198, 18 | 19, _)
                    | (198, 51, 100)
                    | (203, 0, 113)
                    | (224..=255, _, _)
            )
        }
        IpAddr::V6(ip) => {
            let [first, second, ..] = ip.segments();
            // Global-unicast only, excluding address translation, tunneling and documentation.
            (0x2000..0x4000).contains(&first)
                && first != 0x2002
                && !(first == 0x2001 && (second < 0x0200 || second == 0x0db8))
                && !(first == 0x3fff && second < 0x1000)
        }
    }
}

struct Delivery {
    user: String,
    registration: PushRegistration,
    payload: Vec<u8>,
    urgency: &'static str,
}

pub(crate) async fn worker(
    mut signals: mpsc::Receiver<Signal>,
    server: Arc<Server>,
    push: Arc<Push>,
    shutdown: CancellationToken,
) {
    loop {
        let signal = tokio::select! {
            _ = shutdown.cancelled() => break,
            signal = signals.recv() => signal,
        };
        let Some(signal) = signal else { break };
        let deliveries = server.coordinator.lock().await.plan_push(signal);
        for delivery in deliveries {
            let result = tokio::select! {
                _ = shutdown.cancelled() => return,
                result = push.send(&delivery) => result,
            };
            if matches!(result, Ok(StatusCode::NOT_FOUND | StatusCode::GONE)) {
                let mut coordinator = server.coordinator.lock().await;
                let current = coordinator
                    .state
                    .push_registrations
                    .get(&delivery.user)
                    .and_then(|items| items.get(&delivery.registration.url));
                if current.is_some_and(|current| current.version == delivery.registration.version) {
                    let mut proposed = coordinator.state.clone();
                    if let Some(registrations) = proposed.push_registrations.get_mut(&delivery.user)
                    {
                        registrations.remove(&delivery.registration.url);
                    }
                    let _ = coordinator.commit(proposed).await;
                }
            }
        }
    }
}

impl Coordinator {
    pub fn push_signal(&self, signal: Signal) {
        if let Some(queue) = &self.push_queue {
            let _ = queue.try_send(signal);
        }
    }

    fn plan_push(&self, signal: Signal) -> Vec<Delivery> {
        let message = match &signal {
            Signal::Message { id, .. } => self
                .state
                .messages
                .get(id)
                .filter(|message| message["deleted"] != true),
            Signal::Badge(_) => None,
        };
        let room = message.and_then(|message| message["room_id"].as_str());
        let now = epoch_millis();
        let mut deliveries = Vec::new();
        for (user, registrations) in &self.state.push_registrations {
            if let Signal::Badge(target) = &signal
                && target != user
            {
                continue;
            }
            if let Some(message) = message {
                if message["from"]["user_id"] == user.as_str()
                    || !room.is_some_and(|room| visible(&self.state, user, room))
                {
                    continue;
                }
            } else if matches!(signal, Signal::Message { .. }) {
                continue;
            }
            let is_joined = room.is_some_and(|room| joined(&self.state, user, room));
            let muted = status::chosen(&self.state.users[user]) == "dnd"
                || room.is_some_and(|room| is_muted(&self.state, user, room, now));
            let badge_changed = match &signal {
                Signal::Message { badges, .. } => badges.contains(user),
                Signal::Badge(_) => true,
            };
            let unread = unread(&self.state, user);
            for registration in registrations.values() {
                let wake = &registration.wake;
                let priority_scope = message.is_some_and(|message| {
                    wake.contains("private")
                        && is_joined
                        && room.is_some_and(|room| private_room(&self.state, room))
                        || wake.contains("replies")
                            && message["reply_to"]["message_id"]
                                .as_str()
                                .and_then(|id| self.state.messages.get(id))
                                .is_some_and(|reply| reply["from"]["user_id"] == user.as_str())
                        || wake.contains("mentions")
                            && message["body"]["mentions"].as_array().is_some_and(|users| {
                                users.iter().any(|recipient| recipient == user.as_str())
                            })
                });
                let selected = message
                    .filter(|_| !muted && (priority_scope || wake.contains("joined") && is_joined));
                let urgency = if selected.is_some() {
                    if priority_scope { "high" } else { "normal" }
                } else if wake.contains("badge") && badge_changed {
                    "low"
                } else {
                    continue;
                };
                if let Some(payload) = payload(registration, unread, selected) {
                    deliveries.push(Delivery {
                        user: user.clone(),
                        registration: registration.clone(),
                        payload,
                        urgency,
                    });
                }
            }
        }
        deliveries
    }
}

pub(crate) fn badge_changes(previous: &State, next: &State) -> BTreeSet<String> {
    if next.push_registrations.is_empty()
        || previous.watermark == next.watermark && previous.read_cursors == next.read_cursors
    {
        return BTreeSet::new();
    }
    next.push_registrations
        .iter()
        .filter(|(user, registrations)| {
            registrations
                .values()
                .any(|registration| registration.wake.contains("badge"))
                && unread(previous, user) != unread(next, user)
        })
        .map(|(user, _)| user.clone())
        .collect()
}

fn unread(state: &State, user: &str) -> usize {
    let cursors = state.read_cursors.get(user);
    state
        .messages
        .values()
        .filter(|message| {
            let Some(room) = message["room_id"].as_str() else {
                return false;
            };
            let cursor = cursors
                .and_then(|rooms| rooms.get(room))
                .copied()
                .unwrap_or(0);
            message["deleted"] != true
                && message["from"]["user_id"] != user
                && message["message_id"]
                    .as_str()
                    .and_then(|id| id.parse::<u64>().ok())
                    .is_some_and(|id| id > cursor)
                && joined(state, user, room)
                && visible(state, user, room)
        })
        .count()
}

fn is_muted(state: &State, user: &str, room: &str, now: u64) -> bool {
    let Some(scopes) = state.mutes.get(user) else {
        return false;
    };
    if scopes.get("").is_some_and(|expires| *expires > now) {
        return true;
    }
    let mut current = Some(room);
    while let Some(room) = current {
        if scopes.get(room).is_some_and(|expires| *expires > now) {
            return true;
        }
        current = state.rooms[room]["parent_room_id"].as_str();
    }
    false
}

fn private_room(state: &State, room: &str) -> bool {
    let mut current = Some(room);
    while let Some(room) = current {
        if state.rooms[room]["private"] == true {
            return true;
        }
        current = state.rooms[room]["parent_room_id"].as_str();
    }
    false
}

fn payload(
    registration: &PushRegistration,
    unread: usize,
    message: Option<&Value>,
) -> Option<Vec<u8>> {
    let mut payload = json!({"unread": unread});
    if let Some(id) = &registration.push_id {
        payload["push_id"] = json!(id);
    }
    if let Some(message) = message {
        payload["message"] = json!({
            "message_id": message["message_id"], "room_id": message["room_id"],
            "from": {"user_id": message["from"]["user_id"]},
        });
        let limit = 2048usize.checked_sub(serde_json::to_vec(&payload).ok()?.len() + 20)?;
        let original = message["body"]["text"].as_str().unwrap_or_default();
        let mut text = String::new();
        let mut used = 0;
        for character in original.chars() {
            let escaped = match character {
                '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
                '\0'..='\u{1f}' => 6,
                _ => character.len_utf8(),
            };
            if used + escaped > limit {
                break;
            }
            text.push(character);
            used += escaped;
        }
        payload["message"]["body"] = json!({"text": text});
    }
    serde_json::to_vec(&payload)
        .ok()
        .filter(|payload| payload.len() <= 2048)
}
