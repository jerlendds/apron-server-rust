use std::sync::Arc;

use axum::{
    Form, Json,
    extract::{RawQuery, State as AxumState},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use store::{PendingPasskey, epoch_millis};

use super::{
    bearer_user,
    external::{PendingPage, error_page, escape, own_origin, redirect, secure, validate},
    random_secret,
    webauthn::{self, Ceremony, Verified},
};
use crate::{service::Server, wire::Params};

pub(crate) struct Page {
    oauth: PendingPage,
    action: String,
    required_account: Option<String>,
    account: Option<String>,
    ceremony: Option<Ceremony>,
}

pub(crate) struct Addition {
    pub owner: String,
    pub expires: u64,
}

#[derive(Deserialize)]
struct PageQuery {
    action: String,
    addition: Option<String>,
}

pub(crate) async fn show(
    AxumState(server): AxumState<Arc<Server>>,
    RawQuery(query): RawQuery,
) -> Response {
    let query = query.as_deref().unwrap_or_default();
    let Ok(mode) = serde_urlencoded::from_str::<PageQuery>(query) else {
        return error_page(StatusCode::BAD_REQUEST, "Select a passkey action.");
    };
    if !matches!(mode.action.as_str(), "login" | "register") {
        return error_page(StatusCode::BAD_REQUEST, "Invalid passkey action.");
    }
    let mut coordinator = server.coordinator.lock().await;
    if coordinator.passkeys.is_none() {
        return error_page(StatusCode::NOT_FOUND, "Passkey sign-in is unavailable.");
    }
    let (oauth, label) = match validate(query, &coordinator.origin) {
        Ok(result) => result,
        Err(message) => return error_page(StatusCode::BAD_REQUEST, message),
    };
    let now = epoch_millis();
    let required_account = if let Some(ticket) = mode.addition {
        let account = coordinator
            .passkey_additions
            .get(&ticket)
            .filter(|addition| addition.expires > now);
        match account {
            Some(addition) if mode.action == "register" => Some(addition.owner.clone()),
            _ => {
                return error_page(
                    StatusCode::BAD_REQUEST,
                    "This passkey addition request expired.",
                );
            }
        }
    } else {
        None
    };
    coordinator
        .passkey_pages
        .retain(|_, page| page.oauth.expires > now);
    if coordinator.passkey_pages.len() >= 512 {
        return error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in is busy. Try again shortly.",
        );
    }
    let Ok(nonce) = random_secret() else {
        return error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Could not start passkey sign-in.",
        );
    };
    let action = mode.action;
    let token_field = if required_account.is_some() {
        r#"<label>Account token <input type="password" id="token" autocomplete="off" required></label>"#
    } else {
        ""
    };
    coordinator.passkey_pages.insert(
        nonce.clone(),
        Page {
            oauth,
            action: action.clone(),
            required_account,
            account: None,
            ceremony: None,
        },
    );
    let label = escape(&label);
    let origin = escape(&coordinator.origin);
    secure(
        Html(format!(
            r#"<!doctype html>
<html lang="en"><meta charset="utf-8"><title>Apron passkey</title>
<script src="/passkey.js" defer></script>
<main><h1>Apron passkey</h1>
<p><strong>{label}</strong> is requesting access to <strong>{origin}</strong>.</p>
<p>If you allow access, this client can act as you.</p>
<form id="consent" method="post" action="/passkey/finish" data-action="{action}">
<input type="hidden" name="request" value="{nonce}">
<input type="hidden" name="credential" value="">
{token_field}
<p id="account"></p><p id="status" role="status"></p>
<button type="button" id="allow">Allow access with a passkey</button>
<button type="submit" name="decision" value="decline">Decline</button>
<button type="submit" name="decision" value="allow" id="finish" hidden>Continue</button>
</form></main></html>"#
        ))
        .into_response(),
    )
}

#[derive(Deserialize)]
pub(crate) struct Begin {
    request: String,
    #[serde(default)]
    token: String,
}

pub(crate) async fn begin(
    AxumState(server): AxumState<Arc<Server>>,
    headers: HeaderMap,
    Json(form): Json<Begin>,
) -> Response {
    let mut coordinator = server.coordinator.lock().await;
    let Some(passkeys) = coordinator.passkeys.as_ref() else {
        return problem(StatusCode::NOT_FOUND, "Passkey sign-in is unavailable.");
    };
    let origin = coordinator.origin.clone();
    if !own_origin(&headers, &origin) {
        return problem(StatusCode::FORBIDDEN, "Open the Apron passkey page.");
    }
    let Some(page) = coordinator
        .passkey_pages
        .get(&form.request)
        .filter(|page| page.oauth.expires > epoch_millis())
    else {
        return problem(
            StatusCode::BAD_REQUEST,
            "Sign-in request expired. Open the link again.",
        );
    };
    let mut account = None;
    let mut profile = Params::new();
    if let Some(required) = &page.required_account {
        if bearer_user(&coordinator.state, &form.token, Some(&origin)).ok() != Some(required) {
            return problem(
                StatusCode::UNAUTHORIZED,
                "Enter a token for the account that requested the addition.",
            );
        }
        let name = coordinator.state.users[required]["name"]
            .as_str()
            .unwrap_or(required)
            .to_owned();
        profile.insert("name".into(), json!(name));
        account = Some(required.clone());
    }
    let Ok((ceremony, mut result)) = webauthn::begin(
        passkeys,
        &origin,
        &page.action,
        account.as_deref(),
        &profile,
    ) else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "Could not start the passkey ceremony.",
        );
    };
    if let Some(account) = &account {
        result["account"] = coordinator.state.users[account].clone();
    }
    if let Some(page) = coordinator.passkey_pages.get_mut(&form.request) {
        page.ceremony = Some(ceremony);
        page.account = account;
    }
    secure(Json(result).into_response())
}

#[derive(Deserialize)]
pub(crate) struct Finish {
    request: String,
    decision: String,
    credential: String,
}

pub(crate) async fn finish(
    AxumState(server): AxumState<Arc<Server>>,
    headers: HeaderMap,
    Form(form): Form<Finish>,
) -> Response {
    let mut coordinator = server.coordinator.lock().await;
    if !own_origin(&headers, &coordinator.origin) {
        return error_page(StatusCode::FORBIDDEN, "Open the Apron passkey page.");
    }
    let Some(page) = coordinator
        .passkey_pages
        .remove(&form.request)
        .filter(|page| page.oauth.expires > epoch_millis())
    else {
        return error_page(
            StatusCode::BAD_REQUEST,
            "Sign-in request expired. Open the link again.",
        );
    };
    if form.decision == "decline" {
        return redirect(&page.oauth, None, &coordinator.origin);
    }
    let Some(passkeys) = coordinator.passkeys.as_ref() else {
        return error_page(StatusCode::NOT_FOUND, "Passkey sign-in is unavailable.");
    };
    let (Some(ceremony), Ok(credential)) = (
        page.ceremony,
        serde_json::from_str::<Value>(&form.credential),
    ) else {
        return error_page(StatusCode::BAD_REQUEST, "A verified passkey is required.");
    };
    if form.decision != "allow" {
        return error_page(StatusCode::BAD_REQUEST, "Choose whether to allow access.");
    }
    let mut proposed = coordinator.state.clone();
    let verified = webauthn::verify(
        &mut proposed,
        passkeys,
        ceremony,
        &page.action,
        &coordinator.origin,
        page.account.as_deref(),
        &credential,
    );
    let (subject, account, pending_passkey) = match verified {
        Ok(Verified::Login(user)) => (format!("account:{user}"), Some(user), None),
        Ok(Verified::Register { id, key, profile }) => (
            id.clone(),
            page.account,
            Some(PendingPasskey { id, key, profile }),
        ),
        Err(_) => {
            return error_page(
                StatusCode::UNAUTHORIZED,
                "The passkey could not be verified.",
            );
        }
    };
    let method = format!("passkey:{}", page.action);
    match super::external::issue(
        &mut coordinator,
        proposed,
        &page.oauth,
        &method,
        subject,
        account,
        pending_passkey,
    )
    .await
    {
        Ok(code) => redirect(&page.oauth, Some(&code), &coordinator.origin),
        Err(_) => error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Could not complete sign-in.",
        ),
    }
}

pub(crate) async fn script() -> Response {
    secure(
        (
            [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
            include_str!("passkey.js"),
        )
            .into_response(),
    )
}

fn problem(status: StatusCode, message: &str) -> Response {
    secure((status, Json(json!({"error": message}))).into_response())
}
