//! Apron v10 black-box tests for the Rust server. See PROTOCOL.md §§1–4 for
//! the wire contract. Client rendering and Appendix C drafts are outside
//! the server test target.
//!
//! Build and list cases without a daemon:
//! cargo test -p server --test protocol -- --list
//!
//! Run against a disposable server with fixture policy enabled:
//! APRON_TEST_WS=ws://127.0.0.1:9000/ws cargo test -p server --test protocol -- --ignored --test-threads=1
//! A test name filter can be passed before the leading -- to run a subset.
//! Ignored tests fail when explicitly run without the fixture they require.
//!
//! Use a loopback listener, default-room membership for guests, guest
//! signup with resumable bearer tokens, and a fresh non-production store.
//! Enable the capabilities named by the selected tests. The full suite
//! expects private rooms, idle and activity delivery, retry retention,
//! pre-auth welcomes, /help, and push wakes for offline recipients. Use
//! APRON_TEST_CORE_WS for the separate core-only configuration. HTTP tests
//! invoke curl. Set APRON_TEST_EXT_LIMIT to the configured extension byte
//! limit for the extension size test. Provider cases also need
//! APRON_TEST_ORIGIN, APRON_TEST_PUBLIC_WS if it differs from APRON_TEST_WS, and a test
//! APRON_TEST_MAIL_DOMAIN that the mail fixture captures.
//!
//! APRON_TEST_CONTROL is an executable that receives one action argument
//! and a JSON object on stdin. It returns JSON on stdout and exits. The
//! implementation depends on the daemon launch, browser and provider setup.
//! Run it against independent test providers and return their observations.
//!
//! - read-mail(email, state) waits for the matching message and returns
//!   {code?} and/or {link_token, link, state, server}, plus the message
//!   {text}. Decode fragment values from the delivered link. Keep scenarios
//!   state-scoped.
//! - passkey-credential(public_key, origin, action, key, variant) uses a
//!   virtual authenticator and returns the WebAuthn credential JSON, keeping
//!   the authenticator state for each key. Variants are valid, wrong-origin,
//!   wrong-rp, no-user-verification, bad-signature, cross-origin and
//!   top-origin. For bad-signature, alter the assertion while keeping the
//!   registered credential ID. cross-origin sets crossOrigin: true in the
//!   client data; top-origin adds only a topOrigin.
//! - authorize(url, decision, variant, client_id, redirect_uri,
//!   code_challenge, code_challenge_method, state, resource) drives a browser
//!   sign-in and consent. Return actual redirect parameters {code, state, iss}
//!   or {error, state}. Variants are valid, expired, passkey-login and
//!   passkey-register. An optional account_token is entered on the page to
//!   sign in to that account instead of using a new identity.
//!   For expired, advance fixture time past code expiry after consent.
//! - push-sink(kind) provisions an HTTPS receiver and returns {url, token?,
//!   keys?}. For relay-encrypted and webpush supply the receiver's public keys.
//! - take-push(url, wait_ms) removes one captured request and returns {method,
//!   headers, plaintext, decrypted?, vapid_verified?}, or null on timeout.
//!   Use lowercase header names. The plaintext is the exact UTF-8 payload.
//!   Independently decrypt encrypted requests and verify the Web Push VAPID
//!   signature against the server's advertised public key.
//! - push-gone(url, status) makes that receiver answer every later push with
//!   status (404 or 410), still capturing each request with {answered}.
//! - expire-email(email, state), expire-passkey(challenge_id), and
//!   expire-upload(embed_id) wait or advance test time past the relevant
//!   expiry. Return an empty object.
//! - restart() restarts the daemon on the same port and store, waits until
//!   it is ready, and returns an empty object.

mod auth;
mod core;
mod delivery;
mod embeds;
mod history;
mod push;
mod rooms;
mod support;
