![project header](./art/header.svg)

# apron-server-rust

`aprond` is a Rust chat server for [Apron protocol v10](PROTOCOL.md).

<!--
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
It keeps its state in Turso, can serve a web client from the same port, and can fetch its own TLS certificates. -->


<!--


 -->


## Run the server

```sh
cargo run -p aprond -- --listen 127.0.0.1:9000 --db apron.db
```

Connect an Apron v10 client to `ws://127.0.0.1:9000/ws`. The daemon uses Turso
by default. `--memory` runs with ephemeral state, and `--core-only` enables the
minimal WebSocket profile. See `aprond --help` for the environment variables
and options.

Enable email sign-in with `--mail-command PATH`. The executable receives
`--` followed by the recipient and reads an RFC 5322 message on stdin.
Use `--passkey-auth` for WebAuthn and `--external-auth` for the built-in OAuth
authorization page. Enable relay push with `--push-relay` and Web Push with
`--push-vapid-key PATH` pointing to a P-256 PEM private key. Set
`--http-origin https://chat.example` when serving behind a TLS reverse proxy.
The daemon uses this origin in upload grants and sign-in links.

## Run the tests

Unit tests, formatting, and lints run without any setup:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The black-box protocol suite in `crates/server/tests/protocol` is `#[ignore]`d
under plain `cargo test`, because it needs live daemons. Run it with:

```sh
scripts/test-protocol                 # all protocol tests
scripts/test-protocol rooms::         # only tests whose name contains rooms::
scripts/test-protocol --no-browser    # skip the tests that drive Chromium
```

The script builds `aprond`, then `scripts/protocol-fixture.py` starts isolated
loopback daemons, captures email locally, exercises WebAuthn and OAuth sign-in
in headless Chromium, and checks encrypted pushes on a local HTTPS receiver.
It needs Python 3 with `cryptography`, `curl`, and Chromium with its matching
`chromedriver` (not needed with `--no-browser`). Set `APRON_TEST_CHROME` and
`APRON_TEST_CHROMEDRIVER` if they are not on `PATH`.

### Apron conformance

`scripts/conformance` runs the upstream conformance checker,
[`tests/conformance/conform.py`](https://github.com/shazow/apron/blob/main/tests/conformance/conform.py)
from [shazow/apron](https://github.com/shazow/apron), against a fresh
in-memory `aprond`. It checks the core checklist, each capability and sign-in
scheme that `aprond` advertises, and every frame against the protocol's JSON
Schema. The repository's wire fixtures (`tests/fixtures/wire`) test clients,
not servers, so this suite doesn't run them.

The protocol repository lives in `apron/`, which is git-ignored. On first run,
the script fetches it there at the commit pinned in `scripts/conformance`
(`pin=`), the protocol version this server is tested against. `APRON_REF`
overrides it with another commit, branch, or tag. An existing checkout is used
as it is, with a warning when it isn't at the pin. To move to a newer protocol,
check out the commit in `apron/`, get the suite passing, then bump `pin=`. It
needs `uv`, `git`, and `curl`.

The sign-in checks drive a passkey page in Chromium through apron's portal
(`tests/conformance/portals/passkey.cjs`), which needs Node.js and Playwright.
Install Playwright once into the git-ignored `node_modules/`, without writing a
`package.json` or lockfile:

```sh
npm install --no-save --no-package-lock playwright
npx playwright install chromium
```

Then run:

```sh
NODE_PATH="$PWD/node_modules" scripts/conformance
NODE_PATH="$PWD/node_modules" scripts/conformance --only 'core.*'   # arguments go to conform.py
```

Without Playwright, the sign-in checks skip instead; `APRON_REQUIRE_PORTAL=1`
(set in CI) makes that an error. With Playwright, a check that skips because the
portal could not sign in fails the run, so a broken browser can't hide the
sign-in checks. The script prints one line
per check (PASS, FAIL, WARN, SKIP, MANUAL), writes `conformance.json`
(`APRON_CONFORMANCE_JSON` changes the path), and exits 1 on any FAIL. Run
`(cd apron && uv run tests/conformance/conform.py --list)` to list every check.

CI (`.github/workflows/ci.yml`) runs all three: the first block in one job,
the whole protocol suite, browser tests included, in another, and the Apron
conformance suite, with the passkey portal, in a third.
