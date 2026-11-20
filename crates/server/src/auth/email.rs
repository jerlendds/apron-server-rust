use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde_json::{Value, json};
use store::{EmailProposal, State, epoch_millis};
use tokio::{io::AsyncWriteExt, process::Command, sync::mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    service::{ConnectionId, Coordinator},
    wire::{Error, Params, Result, string},
};

use super::{create_account, random_secret, secret_hash, token_result};

const ATTEMPTS: u8 = 4;

#[derive(Clone, Debug)]
pub struct EmailConfig {
    pub command: PathBuf,
    pub ttl_ms: u64,
}

pub(crate) struct Delivery {
    address: String,
    message: String,
}

pub(crate) fn start(
    command: PathBuf,
    shutdown: CancellationToken,
) -> (mpsc::Sender<Delivery>, tokio::task::JoinHandle<()>) {
    let (queue, mut deliveries) = mpsc::channel(32);
    let worker = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                delivery = deliveries.recv() => {
                    let Some(delivery) = delivery else { break };
                    if !matches!(tokio::time::timeout(Duration::from_secs(15), send(&command, delivery)).await, Ok(Ok(()))) {
                        eprintln!("The configured mail transport failed");
                    }
                }
            }
        }
    });
    (queue, worker)
}

async fn send(command: &Path, delivery: Delivery) -> std::io::Result<()> {
    let mut child = Command::new(command)
        .arg("--")
        .arg(delivery.address)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("cannot open mail input"))?;
    input.write_all(delivery.message.as_bytes()).await?;
    drop(input);
    if !child.wait().await?.success() {
        return Err(std::io::Error::other("mail transport exited with an error"));
    }
    Ok(())
}

impl Coordinator {
    pub async fn propose_email(&mut self, connection: ConnectionId, params: &Params) -> Result<()> {
        let sender = self.mail.as_ref().ok_or(Error::Unsupported)?.clone();
        let config = self.config.email.as_ref().ok_or(Error::Unsupported)?;
        let session = self.connections.get(&connection).ok_or(Error::Internal)?;
        let address = normalize(string(params, "email")?.ok_or(Error::Invalid)?)?;
        let supplied_state = proposal_state(params)?.map(str::to_owned);
        let link_token = format!("mail_{}", random_secret()?);
        let key = secret_hash(&link_token);
        let code = short_code()?;
        let now = epoch_millis();
        let proposal = EmailProposal {
            address: address.clone(),
            account: session.user.clone(),
            state: supplied_state,
            code_hash: secret_hash(&format!("{key}:{code}")),
            expires: now.saturating_add(config.ttl_ms),
            attempts: 0,
        };
        let mut proposed = self.state.clone();
        let window = now.saturating_sub(60_000);
        proposed.email_rate.retain(|_, entries| {
            entries.retain(|time| *time > window);
            !entries.is_empty()
        });
        proposed
            .email_proposals
            .retain(|_, pending| pending.expires > now);
        let rate = proposed.email_rate.entry(address.clone()).or_default();
        if rate.len() >= 8 || proposed.email_proposals.len() >= 2048 {
            return Err(Error::RetryAfter(60));
        }
        rate.push(now);
        if let Some(previous) = &session.pending_email {
            proposed.email_proposals.remove(previous);
        }
        let delivery = build_delivery(&self.origin, &proposal, &link_token, &code)?;
        proposed.email_proposals.insert(key.clone(), proposal);
        let slot = sender
            .try_reserve_owned()
            .map_err(|_| Error::RetryAfter(10))?;
        self.commit(proposed).await?;
        self.connections
            .get_mut(&connection)
            .ok_or(Error::Internal)?
            .pending_email = Some(key);
        slot.send(delivery);
        Ok(())
    }
}

fn normalize(address: &str) -> Result<String> {
    let (local, domain) = address.rsplit_once('@').ok_or(Error::Invalid)?;
    if address.len() > 254
        || local.is_empty()
        || local.len() > 64
        || domain.len() > 253
        || !domain.contains('.')
        || !domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !local
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&byte))
    {
        return Err(Error::Invalid);
    }
    Ok(format!("{local}@{}", domain.to_ascii_lowercase()))
}

fn proposal_state(params: &Params) -> Result<Option<&str>> {
    let state = string(params, "state")?;
    if state.is_some_and(|state| state.len() > 256 || state.is_empty()) {
        return Err(Error::Invalid);
    }
    Ok(state)
}

fn short_code() -> Result<String> {
    let mut bytes = [0u8; 4];
    // Rejection sampling avoids bias when reducing a random integer to six digits.
    loop {
        getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
        let value = u32::from_ne_bytes(bytes);
        if value < u32::MAX - u32::MAX % 1_000_000 {
            return Ok(format!("{:06}", value % 1_000_000));
        }
    }
}

fn build_delivery(
    origin: &str,
    proposal: &EmailProposal,
    link_token: &str,
    code: &str,
) -> Result<Delivery> {
    let mut server = Url::parse(origin).map_err(|_| Error::Internal)?;
    let mut link = server.clone();
    link.set_path("/login");
    let scheme = if server.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    server.set_scheme(scheme).map_err(|_| Error::Internal)?;
    server.set_path("/ws");
    let mut fragment = url::form_urlencoded::Serializer::new(String::new());
    fragment.append_pair("token", link_token);
    if let Some(state) = &proposal.state {
        fragment.append_pair("state", state);
    }
    fragment.append_pair("server", server.as_str());
    link.set_fragment(Some(&fragment.finish()));
    Ok(Delivery {
        address: proposal.address.clone(),
        message: format!(
            "To: {}\r\nSubject: Apron sign-in\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n\
             Your code is {code}. Only enter it in a client connected to {server}.\r\n\
             Open {link} to continue.\r\n",
            proposal.address
        ),
    })
}

pub(crate) struct Approval {
    pub outcome: Result<(String, Value, bool)>,
    pub changed: bool,
}

pub(crate) fn approve(
    state: &mut State,
    params: &Params,
    current: Option<&str>,
    pending: Option<&str>,
) -> Approval {
    let mut changed = false;
    let outcome = approve_inner(state, params, current, pending, &mut changed);
    Approval { outcome, changed }
}

fn approve_inner(
    state: &mut State,
    params: &Params,
    current: Option<&str>,
    pending: Option<&str>,
    changed: &mut bool,
) -> Result<(String, Value, bool)> {
    let token = string(params, "token")?.ok_or(Error::Invalid)?;
    let supplied_state = proposal_state(params)?;
    let key = if token.starts_with("mail_") {
        secret_hash(token)
    } else if let Some(pending) = pending.filter(|key| state.email_proposals.contains_key(*key)) {
        pending.to_owned()
    } else if let Some(supplied_state) = supplied_state {
        let mut matches = state
            .email_proposals
            .iter()
            .filter(|(_, proposal)| proposal.state.as_deref() == Some(supplied_state));
        match (matches.next(), matches.next()) {
            (Some((key, _)), None) => key.clone(),
            _ => return Err(Error::Denied),
        }
    } else {
        return Err(Error::Denied);
    };
    let proposal = state
        .email_proposals
        .get(&key)
        .ok_or(Error::Denied)?
        .clone();
    let existing = state.email_addresses.get(&proposal.address).cloned();
    let valid_token = if token.starts_with("mail_") {
        supplied_state.is_none_or(|state| proposal.state.as_deref() == Some(state))
    } else {
        token.len() == 6
            && token.bytes().all(|byte| byte.is_ascii_digit())
            && (pending == Some(key.as_str())
                || supplied_state.is_some_and(|state| proposal.state.as_deref() == Some(state)))
            && proposal.code_hash == secret_hash(&format!("{key}:{token}"))
    };
    let valid_account = match (proposal.account.as_deref(), current) {
        (None, None) => true,
        (Some(account), Some(current)) if account == current => {
            existing.as_deref().is_none_or(|owner| owner == current)
        }
        _ => false,
    };
    if !valid_token || !valid_account || proposal.expires <= epoch_millis() {
        if proposal.attempts + 1 >= ATTEMPTS || proposal.expires <= epoch_millis() {
            state.email_proposals.remove(&key);
        } else {
            state
                .email_proposals
                .get_mut(&key)
                .ok_or(Error::Internal)?
                .attempts += 1;
        }
        *changed = true;
        return Err(Error::Denied);
    }
    let mut created = false;
    let user = match proposal.account.or(existing) {
        Some(user) => user,
        None => {
            created = true;
            create_account(state, params, "user")?
        }
    };
    let result = if current.is_some() {
        json!({"you": state.users[&user]})
    } else {
        token_result(state, &user)?
    };
    state.email_proposals.remove(&key);
    state.email_addresses.insert(proposal.address, user.clone());
    *changed = true;
    Ok((user, result, created))
}

pub(crate) async fn login_page(
    axum::extract::State(server): axum::extract::State<std::sync::Arc<crate::service::Server>>,
) -> axum::response::Response {
    use axum::{
        http::{HeaderValue, StatusCode, header},
        response::{Html, IntoResponse},
    };

    let coordinator = server.coordinator.lock().await;
    if coordinator.mail.is_none() {
        return super::external::error_page(StatusCode::NOT_FOUND, "Email sign-in is unavailable.");
    }
    let ws = if let Some(host) = coordinator.origin.strip_prefix("https://") {
        format!("wss://{host}")
    } else {
        format!(
            "ws://{}",
            coordinator
                .origin
                .strip_prefix("http://")
                .unwrap_or_default()
        )
    };
    let mut page = super::external::secure(Html(r#"<!doctype html>
<html lang="en"><meta charset="utf-8"><title>Apron email sign-in</title>
<script src="/login.js" defer></script>
<main><h1>Apron email sign-in</h1>
<form id="login">
<label>Existing account token <input type="password" name="existing" autocomplete="off"></label>
<p>Enter an existing account token only if you asked to add this email address to that account.</p>
<button type="submit">Confirm email sign-in</button>
</form>
<p id="status" role="status"></p>
<button type="button" id="show-token" hidden>Show token for your Apron client</button>
<label hidden>Session token <input type="text" id="resume-token" readonly autocomplete="off"></label>
</main></html>"#).into_response());
    let policy = format!(
        "default-src 'none'; script-src 'self'; connect-src {ws}; frame-ancestors 'none'; base-uri 'none'"
    );
    if let Ok(policy) = HeaderValue::from_str(&policy) {
        page.headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, policy);
    }
    page
}

pub(crate) async fn login_script() -> axum::response::Response {
    use axum::{http::header, response::IntoResponse};
    super::external::secure(
        (
            [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
            include_str!("login.js"),
        )
            .into_response(),
    )
}
