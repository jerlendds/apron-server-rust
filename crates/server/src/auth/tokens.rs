use serde_json::{Value, json};
use store::State;

use crate::wire::{Error, Result};

pub(crate) fn token_result(state: &mut State, user_id: &str) -> Result<Value> {
    let profile = state.users.get(user_id).ok_or(Error::Internal)?;
    let token = random_secret()?;
    state.tokens.insert(token_key(&token), user_id.to_owned());
    Ok(json!({"you": profile, "token": token}))
}

pub(crate) fn secret_hash(secret: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn random_secret() -> Result<String> {
    use std::fmt::Write;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
    let mut secret = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut secret, "{byte:02x}").map_err(|_| Error::Internal)?;
    }
    Ok(secret)
}

fn token_key(secret: &str) -> String {
    format!("sha256:{}", secret_hash(secret))
}

pub(crate) fn migrate_tokens(state: &mut State) -> bool {
    if state.tokens.keys().all(|key| key.starts_with("sha256:")) {
        return false;
    }
    state.tokens = std::mem::take(&mut state.tokens)
        .into_iter()
        .map(|(key, user)| {
            (
                if key.starts_with("sha256:") {
                    key
                } else {
                    token_key(&key)
                },
                user,
            )
        })
        .collect();
    true
}

pub(crate) fn token_result_bound(state: &mut State, user: &str, origin: &str) -> Result<Value> {
    let now = store::epoch_millis();
    let expired = state
        .token_bindings
        .iter()
        .filter(|(_, binding)| binding.expires <= now)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in expired {
        state.token_bindings.remove(&key);
        state.tokens.remove(&key);
    }
    let result = token_result(state, user)?;
    let token = result["token"].as_str().ok_or(Error::Internal)?;
    state.token_bindings.insert(
        token_key(token),
        store::TokenBinding {
            origin: origin.to_owned(),
            expires: now.saturating_add(86_400_000),
        },
    );
    Ok(result)
}

pub(crate) fn bearer_user<'a>(
    state: &'a State,
    secret: &str,
    origin: Option<&str>,
) -> Result<&'a str> {
    let key = token_key(secret);
    if state.token_bindings.get(&key).is_some_and(|binding| {
        Some(binding.origin.as_str()) != origin || binding.expires <= store::epoch_millis()
    }) {
        return Err(Error::Denied);
    }
    state
        .tokens
        .get(&key)
        .map(String::as_str)
        .ok_or(Error::Denied)
}
