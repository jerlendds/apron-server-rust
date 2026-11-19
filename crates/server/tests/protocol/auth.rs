use super::support::*;
use serde_json::{Value, json};
use std::env;

fn fixture_origin() -> String {
    env::var("APRON_TEST_ORIGIN").expect("set APRON_TEST_ORIGIN to the fixture's public origin")
}

fn direct_peer() -> Peer {
    Peer::at(&env::var("APRON_TEST_WS").unwrap(), Some(&fixture_origin()))
}

fn begin_passkey(peer: &mut Peer, action: &str) -> Value {
    let result = peer
        .auth(json!({"scheme":"webauthn","action":action,"step":"begin"}))
        .ok();
    assert!(result.get("you").is_none());
    opaque(&result["challenge_id"]);
    let options = &result["public_key"];
    let challenge = string(&options["challenge"]);
    assert!(
        challenge
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    );
    if action == "login" {
        assert_eq!(options["userVerification"], "required");
        assert!(items(&options["allowCredentials"]).is_empty());
    } else {
        assert_eq!(options["authenticatorSelection"]["residentKey"], "required");
        assert_eq!(
            options["authenticatorSelection"]["userVerification"],
            "required"
        );
    }
    result
}

fn sign_passkey(result: &Value, action: &str, key: &str, variant: &str) -> Value {
    let credential = control(
        "passkey-credential",
        json!({
            "public_key":result["public_key"], "origin":fixture_origin(),
            "action":action, "key":key, "variant":variant
        }),
    );
    assert!(credential.is_object());
    json!({"scheme":"webauthn","action":action,"step":"finish",
        "challenge_id":result["challenge_id"],"credential":credential})
}

#[test]
#[ignore = "requires direct WebAuthn and a virtual-authenticator fixture"]
fn passkey_registration_login_and_origin_bound_resume() {
    let mut registration = direct_peer();
    registration.scheme("webauthn");
    let owner = registration.guest()["you"]["user_id"].clone();
    let key = unique("passkey");
    let begin = begin_passkey(&mut registration, "register");
    let finish = sign_passkey(&begin, "register", &key, "valid");
    assert_eq!(
        registration.auth(finish.clone()).ok()["you"]["user_id"],
        owner
    );
    registration.auth(finish).error(DENIED);

    let mut login = direct_peer();
    let begin = begin_passkey(&mut login, "login");
    let finish = sign_passkey(&begin, "login", &key, "valid");
    let signed_in = login.auth(finish.clone()).ok();
    assert_eq!(signed_in["you"]["user_id"], owner);
    login.auth(finish).error(DENIED);
    if let Some(token) = signed_in["token"].as_str() {
        let mut without_origin = Peer::open();
        without_origin
            .auth(json!({"scheme":"token","token":token}))
            .error(DENIED);
        let mut same_origin = direct_peer();
        assert_eq!(
            same_origin
                .auth(json!({"scheme":"token","token":token}))
                .ok()["you"]["user_id"],
            owner
        );
    }
}

#[test]
#[ignore = "requires direct WebAuthn and a virtual-authenticator fixture"]
fn passkey_challenges_bind_connection_action_and_verification() {
    let mut owner = direct_peer();
    owner.scheme("webauthn");
    owner.guest();
    let first = begin_passkey(&mut owner, "register");
    let second = begin_passkey(&mut owner, "register");
    assert_ne!(
        first["public_key"]["challenge"],
        second["public_key"]["challenge"]
    );
    assert_ne!(first["challenge_id"], second["challenge_id"]);
    let key = unique("bound");
    owner
        .auth(sign_passkey(&first, "register", &key, "valid"))
        .error(DENIED);

    let mut stranger = direct_peer();
    let valid = sign_passkey(&second, "register", &key, "valid");
    stranger.auth(valid.clone()).error(DENIED);
    let mut wrong_action = valid.clone();
    wrong_action["action"] = json!("login");
    owner.auth(wrong_action).error(DENIED);

    for variant in ["wrong-origin", "wrong-rp", "no-user-verification"] {
        let action = "register";
        let begin = begin_passkey(&mut owner, action);
        let key = unique("invalid");
        let invalid = sign_passkey(&begin, action, &key, variant);
        owner.auth(invalid).error(DENIED);
        let valid = sign_passkey(&begin, action, &key, "valid");
        owner.auth(valid).error(DENIED);
    }
    let key = unique("signature-check");
    let registration = begin_passkey(&mut owner, "register");
    owner
        .auth(sign_passkey(&registration, "register", &key, "valid"))
        .ok();
    let login = begin_passkey(&mut owner, "login");
    owner
        .auth(sign_passkey(&login, "login", &key, "bad-signature"))
        .error(DENIED);
    owner
        .auth(sign_passkey(&login, "login", &key, "valid"))
        .error(DENIED);
}

fn email_address() -> String {
    format!(
        "{}@{}",
        unique("apron"),
        env::var("APRON_TEST_MAIL_DOMAIN").expect("set APRON_TEST_MAIL_DOMAIN")
    )
}

fn propose(peer: &mut Peer, email: &str, state: &str) -> Value {
    assert_eq!(
        peer.auth(json!({"scheme":"email","email":email,"state":state}))
            .ok(),
        json!({})
    );
    control("read-mail", json!({"email":email,"state":state}))
}

fn approve(mail: &Value, state: &str, scheme: &str) -> Value {
    let token = mail["link_token"]
        .as_str()
        .or_else(|| mail["code"].as_str())
        .expect("mail must contain a token");
    if mail["link_token"].is_string() {
        let url = string(&mail["link"]);
        let (base, fragment) = url
            .split_once('#')
            .expect("email token must be in URL fragment");
        assert!(!base.contains("token="));
        for name in ["token", "state", "server"] {
            assert!(
                fragment
                    .split('&')
                    .any(|part| part.starts_with(&format!("{name}=")))
            );
        }
        assert_eq!(mail["state"], state);
        assert_eq!(
            mail["server"],
            env::var("APRON_TEST_PUBLIC_WS").unwrap_or_else(|_| env::var("APRON_TEST_WS").unwrap())
        );
    }
    json!({"scheme":scheme,"token":token,"state":state})
}

#[test]
#[ignore = "requires email authentication and a test mail inbox"]
fn email_proposals_survive_disconnect_and_approve_exactly_once() {
    let mut proposer = Peer::open();
    proposer.scheme("email");
    proposer.scheme("token");
    let state = unique("state");
    let email = email_address();
    let mail = propose(&mut proposer, &email, &state);
    proposer.call("me", json!({})).error(DENIED);
    proposer.close();
    let mut other = Peer::open();
    let approval = approve(&mail, &state, "email");
    let result = other.auth(approval.clone()).ok();
    opaque(&result["you"]["user_id"]);
    let token = string(&result["token"]);
    let mut retry = Peer::open();
    retry.auth(approval).error(DENIED);
    assert_eq!(
        retry.auth(json!({"scheme":"token","token":token})).ok()["you"]["user_id"],
        result["you"]["user_id"]
    );
    let mut known = Peer::open();
    propose(&mut known, &email, &unique("known-state"));
}

#[test]
#[ignore = "requires email authentication and a test mail inbox"]
fn email_replacement_short_code_and_account_addition_bindings() {
    let mut owner = Peer::open();
    let user = owner.guest()["you"]["user_id"].clone();
    let state = unique("first-state");
    let first = propose(&mut owner, &email_address(), &state);
    let state2 = unique("second-state");
    let email = email_address();
    let second = propose(&mut owner, &email, &state2);
    owner.auth(approve(&first, &state, "email")).error(DENIED);
    let mut outsider = Peer::open();
    let mut other_account = Peer::open();
    other_account.guest();
    let approval = approve(&second, &state2, "token");
    outsider.auth(approval.clone()).error(DENIED);
    other_account.auth(approval.clone()).error(DENIED);

    // A failed approval can count against its proposal. Get a fresh proposal
    // for the success check so it is independent of the configured limit.
    let addition_state = unique("addition");
    let second = propose(&mut owner, &email, &addition_state);
    assert_eq!(
        owner.auth(approve(&second, &addition_state, "token")).ok()["you"]["user_id"],
        user
    );
    let signin_state = unique("signin");
    let mail = propose(&mut outsider, &email, &signin_state);
    if let Some(code) = mail["code"].as_str() {
        Peer::open()
            .auth(json!({"scheme":"email","token":code}))
            .error(DENIED);
    }
    other_account
        .auth(approve(&mail, &signin_state, "email"))
        .error(DENIED);
}

const CLIENT: &str = "chat.apron.conformance";
const REDIRECT: &str = "chat.apron.conformance:/signed-in";
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn external_url(peer: &mut Peer) -> String {
    peer.scheme("external");
    peer.scheme("token");
    let started = peer.auth(json!({"scheme":"external"})).ok();
    assert!(started.get("you").is_none());
    let url = string(&started["external"]["url"]);
    assert!(
        url.starts_with("https://")
            || url.starts_with("http://127.0.0.1:")
            || url.starts_with("http://[::1]:")
            || url.starts_with("http://localhost:")
    );
    url.to_owned()
}

fn authorize(url: &str, decision: &str, variant: &str) -> Value {
    let state = unique("oauth");
    let result = control(
        "authorize",
        json!({"url":url,"decision":decision,"variant":variant,
        "client_id":CLIENT,"redirect_uri":REDIRECT,"code_challenge":CHALLENGE,
        "code_challenge_method":"S256","state":state,"resource":fixture_origin()}),
    );
    assert_eq!(result["state"], state);
    if decision == "allow" {
        string(&result["code"]);
        string(&result["iss"]);
    } else {
        assert_eq!(result["error"], "access_denied");
        assert!(result.get("code").is_none());
    }
    result
}

fn redemption(scheme: &str, code: &Value) -> Value {
    json!({"scheme":scheme,"code":code,"code_verifier":VERIFIER,"redirect_uri":REDIRECT,"client_id":CLIENT})
}

#[test]
#[ignore = "requires external auth and browser authorization fixture"]
fn external_code_is_single_use_and_accepts_resume_without_origin_binding() {
    let mut peer = Peer::open();
    let url = external_url(&mut peer);
    authorize(&url, "decline", "valid");
    let code = authorize(&url, "allow", "valid")["code"].clone();
    let result = peer.auth(redemption("external", &code)).ok();
    opaque(&result["you"]["user_id"]);
    let token = string(&result["token"]);
    let mut retry = Peer::open();
    retry.auth(redemption("external", &code)).error(DENIED);
    let mut cross_origin = Peer::at(
        &env::var("APRON_TEST_WS").unwrap(),
        Some("https://fixture-client.invalid"),
    );
    assert_eq!(
        cross_origin
            .auth(json!({"scheme":"token","token":token}))
            .ok()["you"]["user_id"],
        result["you"]["user_id"]
    );
}

#[test]
#[ignore = "requires external auth and browser authorization fixture"]
fn external_redemption_rejects_wrong_pkce_client_redirect_and_expired_code() {
    let mut peer = Peer::open();
    let url = external_url(&mut peer);
    for (key, value) in [
        (
            "code_verifier",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        ("client_id", "chat.apron.wrong"),
        ("redirect_uri", "chat.apron.conformance:/wrong"),
    ] {
        let code = authorize(&url, "allow", "valid")["code"].clone();
        let mut request = redemption("external", &code);
        request[key] = json!(value);
        peer.auth(request).error(DENIED);
    }
    let expired = authorize(&url, "allow", "expired")["code"].clone();
    peer.auth(redemption("external", &expired)).error(DENIED);
    peer.auth(redemption("external", &json!(unique("invalid"))))
        .error(DENIED);
}

#[test]
#[ignore = "requires a local self-hosted authorization endpoint and curl"]
fn authorization_page_rejects_invalid_requests_at_its_own_origin() {
    let mut peer = Peer::open();
    let url = external_url(&mut peer);
    for (redirect, method, resource) in [
        ("javascript%3Aalert%281%29", "S256", Some(fixture_origin())),
        (
            "https%3A%2F%2Fclient.invalid%2Foauth",
            "plain",
            Some(fixture_origin()),
        ),
        (
            "https%3A%2F%2Fclient.invalid%2Foauth",
            "S256",
            Some("https://wrong.invalid".into()),
        ),
        ("https%3A%2F%2Fclient.invalid%2Foauth", "S256", None),
    ] {
        let delimiter = if url.contains('?') { '&' } else { '?' };
        let mut request = format!(
            "{url}{delimiter}response_type=code&client_id={CLIENT}&redirect_uri={redirect}&code_challenge={CHALLENGE}&code_challenge_method={method}&state=fixture-state"
        );
        if let Some(resource) = resource {
            request.push_str(&format!("&resource={resource}"));
        }
        let response = http("GET", &request, &[], None);
        assert!(!(300..400).contains(&response.status));
        assert!(response.header("Location").is_none());
        assert!(matches!(
            response.header("Cross-Origin-Opener-Policy"),
            None | Some("unsafe-none")
        ));
        assert!(!response.body.is_empty());
    }
}

#[test]
#[ignore = "requires passkey page and browser authorization fixture"]
fn passkey_page_codes_are_action_specific_and_single_use() {
    let mut peer = Peer::open();
    peer.scheme("webauthn");
    let begin = peer
        .auth(json!({"scheme":"webauthn","action":"login","step":"begin"}))
        .ok();
    let url = string(&begin["external"]["url"]);
    let first = authorize(url, "allow", "passkey-login")["code"].clone();
    peer.auth(redemption("external", &first)).error(DENIED);
    let second = authorize(url, "allow", "passkey-login")["code"].clone();
    let mut finish = redemption("webauthn", &second);
    finish["step"] = json!("finish");
    finish["action"] = json!("register");
    peer.auth(finish).error(DENIED);
    let code = authorize(url, "allow", "passkey-login")["code"].clone();
    let mut finish = redemption("webauthn", &code);
    finish["step"] = json!("finish");
    finish["action"] = json!("login");
    let result = peer.auth(finish.clone()).ok();
    opaque(&result["you"]["user_id"]);
    Peer::open().auth(finish).error(DENIED);
    if let Some(token) = result["token"].as_str() {
        assert_eq!(
            Peer::open()
                .auth(json!({"scheme":"token","token":token}))
                .ok()["you"]["user_id"],
            result["you"]["user_id"]
        );
    }
}

#[test]
#[ignore = "requires email and direct WebAuthn with fixture-controlled credential expiry"]
fn expired_email_tokens_and_passkey_challenges_are_denied() {
    let mut email = Peer::open();
    let address = email_address();
    let state = unique("expiry-state");
    let proposed = propose(&mut email, &address, &state);
    control("expire-email", json!({"email":address,"state":state}));
    Peer::open()
        .auth(approve(&proposed, &state, "email"))
        .error(DENIED);

    let mut passkey = direct_peer();
    passkey.guest();
    let begin = begin_passkey(&mut passkey, "register");
    let finish = sign_passkey(&begin, "register", &unique("expired"), "valid");
    control(
        "expire-passkey",
        json!({"challenge_id":begin["challenge_id"]}),
    );
    passkey.auth(finish).error(DENIED);
}
