use std::sync::Arc;

use axum::{
    Form,
    extract::{RawQuery, State as AxumState},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use store::{AuthorizationBinding, AuthorizationCode, PendingPasskey, State, epoch_millis};
use url::{Host, Url};

use crate::{
    service::{Coordinator, Server},
    wire::{Error, Params, Result, string},
};

use super::{bearer_user, create_account, random_secret, secret_hash, token_result};

const PAGE_TTL_MS: u64 = 10 * 60 * 1000;

pub(crate) struct PendingPage {
    binding: AuthorizationBinding,
    state: String,
    pub expires: u64,
}

pub(crate) fn begin(origin: &str, params: &Params) -> Result<Value> {
    if params.contains_key("token") {
        return Err(Error::Denied);
    }
    Ok(json!({"external": {"url": format!("{origin}/oauth/authorize")}}))
}

pub(crate) fn redeem(
    state: &mut State,
    params: &Params,
    current: Option<&str>,
    origin: &str,
    method: &str,
) -> Result<(String, Value, bool)> {
    let code = string(params, "code")?.ok_or(Error::Denied)?;
    let client = string(params, "client_id")?.ok_or(Error::Denied)?;
    let redirect = string(params, "redirect_uri")?.ok_or(Error::Denied)?;
    let verifier = string(params, "code_verifier")?.ok_or(Error::Denied)?;
    if !(43..=128).contains(&verifier.len())
        || !verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
    {
        return Err(Error::Denied);
    }
    let key = secret_hash(code);
    let code = state
        .authorization_codes
        .get(&key)
        .ok_or(Error::Denied)?
        .clone();
    if code.expires <= epoch_millis()
        || code.method != method
        || code.binding.resource != origin
        || code.binding.client_id != client
        || code.binding.redirect_uri != redirect
        || code.binding.code_challenge != URL_SAFE_NO_PAD.encode(Sha256::digest(verifier))
    {
        return Err(Error::Denied);
    }
    let identity = format!("{origin}\n{}", code.subject);
    let existing = code
        .account
        .as_deref()
        .or_else(|| state.external_identities.get(&identity).map(String::as_str));
    let (user, created) = if let Some(pending) = code.pending_passkey {
        if current != code.account.as_deref() {
            return Err(Error::Denied);
        }
        super::webauthn::Verified::Register {
            id: pending.id,
            key: pending.key,
            profile: pending.profile,
        }
        .finalize(state)?
    } else {
        match (existing, current) {
            (Some(owner), Some(current)) if owner != current && method == "external" => {
                return Err(Error::Denied);
            }
            (Some(owner), _) => (owner.to_owned(), false),
            (None, Some(current)) => (current.to_owned(), false),
            (None, None) => (create_account(state, params, "user")?, true),
        }
    };
    state.authorization_codes.remove(&key);
    if method == "external" {
        state.external_identities.insert(identity, user.clone());
    }
    let result = if current == Some(user.as_str()) && method != "passkey:login" {
        json!({"you": state.users.get(&user).ok_or(Error::Internal)?})
    } else {
        token_result(state, &user)?
    };
    Ok((user, result, created))
}

pub(crate) async fn show(
    AxumState(server): AxumState<Arc<Server>>,
    RawQuery(query): RawQuery,
) -> Response {
    let mut coordinator = server.coordinator.lock().await;
    if !coordinator.config.external {
        return error_page(StatusCode::NOT_FOUND, "External sign-in is unavailable.");
    }
    let (page, client_label) =
        match validate(query.as_deref().unwrap_or_default(), &coordinator.origin) {
            Ok(page) => page,
            Err(reason) => return error_page(StatusCode::BAD_REQUEST, reason),
        };
    let nonce = match create_page(&mut coordinator, page) {
        Ok(nonce) => nonce,
        Err(_) => {
            return error_page(
                StatusCode::SERVICE_UNAVAILABLE,
                "Sign-in is busy. Try again shortly.",
            );
        }
    };
    let origin = escape(&coordinator.origin);
    let client_label = escape(&client_label);
    secure(Html(format!(r#"<!doctype html>
<html lang="en"><meta charset="utf-8"><title>Apron sign-in</title>
<main><h1>Apron sign-in</h1>
<p><strong>{client_label}</strong> is requesting access to <strong>{origin}</strong>.</p>
<p>If you allow access, this client can act as you.</p>
<form method="post">
<input type="hidden" name="request" value="{nonce}">
<label>Existing account token (optional) <input type="password" name="token" autocomplete="off"></label>
<p>Leave the token blank to use a new identity.</p>
<button type="submit" name="decision" value="allow">Allow access</button>
<button type="submit" name="decision" value="decline">Decline</button>
</form></main></html>"#)).into_response())
}

#[derive(Deserialize)]
pub(crate) struct Consent {
    request: String,
    decision: String,
    #[serde(default)]
    token: String,
}

pub(crate) async fn consent(
    AxumState(server): AxumState<Arc<Server>>,
    headers: HeaderMap,
    Form(form): Form<Consent>,
) -> Response {
    let mut coordinator = server.coordinator.lock().await;
    if !coordinator.config.external {
        return error_page(StatusCode::NOT_FOUND, "External sign-in is unavailable.");
    }
    if !own_origin(&headers, &coordinator.origin) {
        return error_page(
            StatusCode::FORBIDDEN,
            "Consent must come from the Apron sign-in page.",
        );
    }
    let Some(page) = coordinator
        .browser_pages
        .remove(&form.request)
        .filter(|page| page.expires > epoch_millis())
    else {
        return error_page(
            StatusCode::BAD_REQUEST,
            "Sign-in request expired. Open the link again.",
        );
    };
    let identity = if form.decision == "allow" {
        if form.token.is_empty() {
            random_secret().map(|subject| (subject, None))
        } else {
            bearer_user(&coordinator.state, &form.token, None)
                .map(|user| (format!("account:{user}"), Some(user.to_owned())))
        }
    } else if form.decision == "decline" {
        return redirect(&page, None, &coordinator.origin);
    } else {
        return error_page(StatusCode::BAD_REQUEST, "Choose whether to allow access.");
    };
    let (subject, account) = match identity {
        Ok(identity) => identity,
        Err(_) => {
            return error_page(
                StatusCode::UNAUTHORIZED,
                "The sign-in token was declined. Open the link to try again.",
            );
        }
    };
    let proposed = coordinator.state.clone();
    match issue(
        &mut coordinator,
        proposed,
        &page,
        "external",
        subject,
        account,
        None,
    )
    .await
    {
        Ok(code) => redirect(&page, Some(&code), &coordinator.origin),
        Err(_) => error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Could not complete sign-in.",
        ),
    }
}

pub(crate) fn create_page(coordinator: &mut Coordinator, page: PendingPage) -> Result<String> {
    let now = epoch_millis();
    coordinator
        .browser_pages
        .retain(|_, request| request.expires > now);
    if coordinator.browser_pages.len() >= 512 {
        return Err(Error::RetryAfter(30));
    }
    let nonce = random_secret()?;
    coordinator.browser_pages.insert(nonce.clone(), page);
    Ok(nonce)
}

pub(crate) async fn issue(
    coordinator: &mut Coordinator,
    mut proposed: State,
    page: &PendingPage,
    method: &str,
    subject: String,
    account: Option<String>,
    pending_passkey: Option<PendingPasskey>,
) -> Result<String> {
    let code = random_secret()?;
    let now = epoch_millis();
    proposed
        .authorization_codes
        .retain(|_, code| code.expires > now);
    if proposed.authorization_codes.len() >= 2048 {
        return Err(Error::RetryAfter(30));
    }
    proposed.authorization_codes.insert(
        secret_hash(&code),
        AuthorizationCode {
            binding: page.binding.clone(),
            pending_passkey,
            method: method.to_owned(),
            subject,
            account,
            expires: now.saturating_add(coordinator.config.code_ttl_ms),
        },
    );
    coordinator.commit(proposed).await?;
    Ok(code)
}

#[derive(Deserialize)]
struct Query {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    code_challenge_method: String,
    resource: String,
    state: String,
}

pub(crate) fn validate(
    query: &str,
    origin: &str,
) -> std::result::Result<(PendingPage, String), &'static str> {
    let fields: Query = serde_urlencoded::from_str(query)
        .map_err(|_| "Missing or invalid authorization fields.")?;
    if fields.response_type != "code"
        || fields.code_challenge_method != "S256"
        || fields.code_challenge.len() != 43
        || URL_SAFE_NO_PAD
            .decode(&fields.code_challenge)
            .map_or(true, |bytes| bytes.len() != 32)
    {
        return Err("Authorization requires an S256 PKCE challenge.");
    }
    if fields.resource != origin {
        return Err("The authorization resource must be this Apron origin.");
    }
    if fields.state.is_empty()
        || fields.state.len() > 512
        || fields.client_id.is_empty()
        || fields.client_id.len() > 512
        || fields.redirect_uri.len() > 2048
        || fields.redirect_uri.chars().any(char::is_control)
    {
        return Err("Invalid authorization state, client, or redirect URI.");
    }
    let target = Url::parse(&fields.redirect_uri).map_err(|_| "Invalid redirect URI.")?;
    if !target.username().is_empty()
        || target.password().is_some()
        || target.fragment().is_some()
        || target
            .query_pairs()
            .any(|(name, _)| matches!(name.as_ref(), "code" | "error" | "state" | "iss"))
    {
        return Err("Invalid redirect URI.");
    }
    let label = match target.scheme() {
        "https" if target.has_host() => target.host_str().unwrap_or_default().to_owned(),
        "http" if loopback(&target) => target.host_str().unwrap_or_default().to_owned(),
        scheme if scheme.contains('.') => scheme.to_owned(),
        _ => {
            return Err(
                "The redirect URI must use HTTPS, a loopback HTTP address, or an app scheme.",
            );
        }
    };
    Ok((
        PendingPage {
            state: fields.state,
            binding: AuthorizationBinding {
                client_id: fields.client_id,
                redirect_uri: fields.redirect_uri,
                code_challenge: fields.code_challenge,
                resource: fields.resource,
            },
            expires: epoch_millis().saturating_add(PAGE_TTL_MS),
        },
        label,
    ))
}

pub(crate) fn loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain("localhost")) => true,
        _ => false,
    }
}

pub(crate) fn redirect(page: &PendingPage, code: Option<&str>, origin: &str) -> Response {
    let Ok(mut target) = Url::parse(&page.binding.redirect_uri) else {
        return error_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not complete sign-in.",
        );
    };
    {
        let mut query = target.query_pairs_mut();
        match code {
            Some(code) => {
                query.append_pair("code", code);
            }
            None => {
                query.append_pair("error", "access_denied");
            }
        }
        query
            .append_pair("state", &page.state)
            .append_pair("iss", origin);
    }
    let Ok(location) = HeaderValue::from_str(target.as_str()) else {
        return error_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not complete sign-in.",
        );
    };
    secure((StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response())
}

pub(crate) fn error_page(status: StatusCode, reason: &str) -> Response {
    secure((status, Html(format!(
        "<!doctype html><html lang=en><meta charset=utf-8><title>Apron sign-in</title><h1>Apron sign-in</h1><p>{}</p>",
        escape(reason)
    ))).into_response())
}

pub(crate) fn secure(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'none'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'"
    ));
    response
}

pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(crate) fn own_origin(headers: &HeaderMap, origin: &str) -> bool {
    let supplied = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    supplied == Some(origin)
        // Form navigation under Referrer-Policy: no-referrer can send a null Origin.
        // Require browser-provided same-origin metadata and a separate one-use nonce.
        || supplied == Some("null")
            && headers.get("sec-fetch-site").is_some_and(|value| value == "same-origin")
}
