use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use store::{State, StoredPasskey, epoch_millis};
use webauthn_rs::prelude::*;

use crate::{
    service::{ConnectionId, Coordinator},
    wire::{Error, Params, Result, string},
};

use super::{create_account, external, random_secret, token_result_bound};

pub(crate) struct Passkeys {
    pub library: Webauthn,
    pub ttl_ms: u64,
}

impl Passkeys {
    pub fn new(origin: &Url, ttl_ms: u64, title: &str) -> std::result::Result<Self, WebauthnError> {
        let host = origin.host_str().ok_or(WebauthnError::Configuration)?;
        let passkeys = WebauthnBuilder::new(host, origin)?
            .rp_name(title)
            .timeout(Duration::from_millis(ttl_ms))
            .build()?;
        Ok(Self {
            library: passkeys,
            ttl_ms,
        })
    }
}

pub(crate) struct Ceremony {
    id: String,
    origin: String,
    expires: u64,
    state: CeremonyState,
}

enum CeremonyState {
    Register {
        registration: PasskeyRegistration,
        handle: Uuid,
        account: Option<String>,
        profile: Params,
    },
    Login(DiscoverableAuthentication),
}

pub(crate) fn begin(
    passkeys: &Passkeys,
    origin: &str,
    action: &str,
    account: Option<&str>,
    params: &Params,
) -> Result<(Ceremony, Value)> {
    let (state, options) = match action {
        "register" => {
            let handle = Uuid::new_v4();
            let name = string(params, "name")?
                .filter(|name| !name.is_empty())
                .or(account)
                .unwrap_or("Apron account");
            if name.len() > 512 {
                return Err(Error::TooLarge);
            }
            let (options, registration) = passkeys
                .library
                .start_passkey_registration(handle, name, name, None)
                .map_err(|_| Error::Internal)?;
            let mut options = serde_json::to_value(options).map_err(|_| Error::Internal)?;
            options["publicKey"]["authenticatorSelection"]["residentKey"] = json!("required");
            options["publicKey"]["authenticatorSelection"]["requireResidentKey"] = json!(true);
            (
                CeremonyState::Register {
                    registration,
                    handle,
                    account: account.map(str::to_owned),
                    profile: params.clone(),
                },
                options,
            )
        }
        "login" => {
            let (options, authentication) = passkeys
                .library
                .start_discoverable_authentication()
                .map_err(|_| Error::Internal)?;
            (
                CeremonyState::Login(authentication),
                serde_json::to_value(options).map_err(|_| Error::Internal)?,
            )
        }
        _ => return Err(Error::Invalid),
    };
    let id = random_secret()?;
    let result = json!({"challenge_id": id, "public_key": options["publicKey"]});
    Ok((
        Ceremony {
            id,
            origin: origin.to_owned(),
            expires: epoch_millis().saturating_add(passkeys.ttl_ms),
            state,
        },
        result,
    ))
}

pub(crate) enum Verified {
    Register {
        id: String,
        key: StoredPasskey,
        profile: Params,
    },
    Login(String),
}

impl Verified {
    pub fn finalize(self, state: &mut State) -> Result<(String, bool)> {
        match self {
            Self::Login(user) => Ok((user, false)),
            Self::Register {
                id,
                mut key,
                profile,
            } => {
                if state.passkeys.contains_key(&id) {
                    return Err(Error::Denied);
                }
                let created = key.user.is_empty();
                if created {
                    key.user = create_account(state, &profile, "user")?;
                }
                let user = key.user.clone();
                state.passkeys.insert(id, key);
                Ok((user, created))
            }
        }
    }
}

pub(crate) fn verify(
    state: &mut State,
    passkeys: &Passkeys,
    ceremony: Ceremony,
    action: &str,
    origin: &str,
    account: Option<&str>,
    credential: &Value,
) -> Result<Verified> {
    if ceremony.origin != origin || ceremony.expires <= epoch_millis() {
        return Err(Error::Denied);
    }
    let client_data = credential["response"]["clientDataJSON"]
        .as_str()
        .ok_or(Error::Denied)?;
    let client_data = URL_SAFE_NO_PAD
        .decode(client_data)
        .map_err(|_| Error::Denied)?;
    let client_data: Value = serde_json::from_slice(&client_data).map_err(|_| Error::Denied)?;
    if client_data["origin"] != origin
        || client_data.get("topOrigin").is_some()
        || !matches!(
            client_data.get("crossOrigin"),
            None | Some(Value::Bool(false))
        )
    {
        return Err(Error::Denied);
    }
    match (action, ceremony.state) {
        (
            "register",
            CeremonyState::Register {
                registration,
                handle,
                account: owner,
                profile,
            },
        ) => {
            if owner.as_deref() != account
                || credential["clientExtensionResults"]["credProps"]["rk"] == false
            {
                return Err(Error::Denied);
            }
            let credential: RegisterPublicKeyCredential =
                serde_json::from_value(credential.clone()).map_err(|_| Error::Denied)?;
            let key = passkeys
                .library
                .finish_passkey_registration(&credential, &registration)
                .map_err(|_| Error::Denied)?;
            let id = URL_SAFE_NO_PAD.encode(&**key.cred_id());
            if state.passkeys.contains_key(&id) {
                return Err(Error::Denied);
            }
            let key = StoredPasskey {
                user: owner.unwrap_or_default(),
                handle: handle.to_string(),
                credential: serde_json::to_value(key).map_err(|_| Error::Internal)?,
            };
            Ok(Verified::Register { id, key, profile })
        }
        ("login", CeremonyState::Login(authentication)) => {
            let credential: PublicKeyCredential =
                serde_json::from_value(credential.clone()).map_err(|_| Error::Denied)?;
            let (handle, id) = passkeys
                .library
                .identify_discoverable_authentication(&credential)
                .map_err(|_| Error::Denied)?;
            let id = URL_SAFE_NO_PAD.encode(id);
            let stored = state.passkeys.get_mut(&id).ok_or(Error::Denied)?;
            if stored.handle != handle.to_string() {
                return Err(Error::Denied);
            }
            let mut key: Passkey =
                serde_json::from_value(stored.credential.clone()).map_err(|_| Error::Internal)?;
            let result = passkeys
                .library
                .finish_discoverable_authentication_v2(&credential, authentication, &key)
                .map_err(|_| Error::Denied)?;
            if !result.user_verified() {
                return Err(Error::Denied);
            }
            if key.update_credential(&result) == Some(true) {
                stored.credential = serde_json::to_value(key).map_err(|_| Error::Internal)?;
            }
            Ok(Verified::Login(stored.user.clone()))
        }
        _ => Err(Error::Denied),
    }
}

pub(crate) enum Outcome {
    Pending(Value),
    SignedIn(String, Value, bool),
}

impl Coordinator {
    pub fn passkey(
        &mut self,
        state: &mut State,
        connection: ConnectionId,
        params: &Params,
    ) -> Result<Outcome> {
        let passkeys = self.passkeys.as_ref().ok_or(Error::Unsupported)?;
        let action = string(params, "action")?.ok_or(Error::Invalid)?;
        if !matches!(action, "login" | "register") {
            return Err(Error::Invalid);
        }
        let step = string(params, "step")?.ok_or(Error::Invalid)?;
        let session = self
            .connections
            .get_mut(&connection)
            .ok_or(Error::Internal)?;
        match step {
            "begin" => {
                let mut result = json!({"external": {"url": format!("{}/passkey?action={action}", self.origin)}});
                if action == "register"
                    && let Some(user) = &session.user
                {
                    let now = epoch_millis();
                    self.passkey_additions
                        .retain(|_, addition| addition.expires > now);
                    if self.passkey_additions.len() >= 2048 {
                        return Err(Error::RetryAfter(30));
                    }
                    let ticket = random_secret()?;
                    self.passkey_additions.insert(
                        ticket.clone(),
                        super::passkey_page::Addition {
                            owner: user.clone(),
                            expires: now.saturating_add(600_000),
                        },
                    );
                    result["external"]["url"] = json!(format!(
                        "{}/passkey?action=register&addition={ticket}",
                        self.origin
                    ));
                }
                session.ceremony = None;
                if session.origin.as_deref() == Some(self.origin.as_str()) {
                    let (ceremony, direct) = begin(
                        passkeys,
                        &self.origin,
                        action,
                        session.user.as_deref(),
                        params,
                    )?;
                    session.ceremony = Some(ceremony);
                    result["challenge_id"] = direct["challenge_id"].clone();
                    result["public_key"] = direct["public_key"].clone();
                }
                Ok(Outcome::Pending(result))
            }
            "finish" if params.contains_key("code") => {
                let method = format!("passkey:{action}");
                let (user, result, created) = external::redeem(
                    state,
                    params,
                    session.user.as_deref(),
                    &self.origin,
                    &method,
                )?;
                Ok(Outcome::SignedIn(user, result, created))
            }
            "finish" => {
                let challenge = string(params, "challenge_id")?.ok_or(Error::Denied)?;
                if session
                    .ceremony
                    .as_ref()
                    .is_none_or(|ceremony| ceremony.id != challenge)
                {
                    return Err(Error::Denied);
                }
                let ceremony = session.ceremony.take().ok_or(Error::Denied)?;
                let origin = session.origin.as_deref().ok_or(Error::Denied)?;
                let credential = params.get("credential").ok_or(Error::Denied)?;
                let verified = verify(
                    state,
                    passkeys,
                    ceremony,
                    action,
                    origin,
                    session.user.as_deref(),
                    credential,
                )?;
                let (user, created) = verified.finalize(state)?;
                let result = token_result_bound(state, &user, origin)?;
                Ok(Outcome::SignedIn(user, result, created))
            }
            _ => Err(Error::Invalid),
        }
    }
}
