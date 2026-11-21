![project header](./art/header.svg)

# apron-server-rust (*work-in-progress*)

`aprond` is a single Rust binary that implements every capability of [Apron protocol v10](https://github.com/shazow/apron/blob/096e2fe04ce7d3eaabf0cc748285abef5a694ae3/HISTORY.md).
- rooms
- threads
- private rooms
- full history
- edits and replies
- reactions and custom emoji
- file uploads
- live terminal output and text streamed into messages
- commands
- presence status and mutes
- Web Push and relay push notifications
- roles
- passkey, email, and external (OAuth) sign-in
It keeps its state in Turso, can serve a web client from the same port, and can fetch its own TLS certificates.

## Run the server

```sh
cargo run -p aprond -- --listen 127.0.0.1:9000 --db apron.db
```

### Server configuration 

- `--memory` runs with ephemeral state. The daemon uses Turso by default.
- `--core-only` enables the minimal WebSocket profile.
- `--passkey-auth` for WebAuthn
- `--external-auth` for the built-in OAuth authorization page
- Enable relay push with `--push-relay` and Web Push with `--push-vapid-key PATH` pointing to a P-256 PEM private key.
- `--mail-command PATH` Enable email sign-in 
- `--http-origin https://chat.example` when serving behind a TLS reverse proxy. The daemon uses this origin in upload grants and sign-in links.

## Apron conformance

`scripts/conformance` runs the upstream conformance checker,
[`tests/conformance/conform.py`](https://github.com/shazow/apron/blob/main/tests/conformance/conform.py)
from [shazow/apron](https://github.com/shazow/apron), against a fresh
in-memory `aprond`. It checks the core checklist, each capability and sign-in
scheme that `aprond` advertises, and every frame against the protocol's JSON
Schema.
