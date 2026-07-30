# Gabriel

A local-first API workbench built around one loop:

```text
browse → capture → promote → replay (with the session intact) → diff
```

The [strategy document](docs/gabriel-browser-strategy.md) argues that the defensible
part of a "developer browser" is not the browser — it is the seam between the
browser, the API bench, and the proxy. This repository builds that seam, headless,
as the engine a browser shell would later sit on top of.

## What works today

```bash
gabriel init                       # a collection is a folder of TOML files
gabriel capture start              # local proxy; every request is recorded
gabriel capture ls                 # find the request you care about
gabriel promote <capture-id>       # it becomes an editable, committable file
gabriel run <name>                 # replay it — still authenticated
gabriel diff <id-a> <id-b>         # what changed between two responses
```

The promotion step is the point. A captured request becomes a file with **no
credentials in it**: the `Cookie` header is replaced by a reference to the
session the browser established, and a captured bearer token moves into the
encrypted vault. The file is safe to commit; the replay is still authenticated.

```toml
# gabriel/requests/users/cookies.toml — written by `gabriel promote`
name = "GET cookies"
method = "GET"
url = "{{base_url}}/cookies"

[auth]
type = "session"        # replays with the browser's cookies, not a copied token
session = "work"

[origin]
capture = "c19faf40372f0002"
promoted_at = 1785351712643
```

## Layout

```text
crates/
  gabriel-core        the file format, template resolution, captures, diff
  gabriel-collection  collections on disk: discovery, environments, defaults
  gabriel-vault       XChaCha20-Poly1305 secret store (OS keychain or Argon2id)
  gabriel-engine      request execution, auth, cookie jars, assertions
  gabriel-proxy       MITM capture proxy with a per-install CA
  gabriel-cli         the `gabriel` binary
```

A collection is a directory:

```text
gabriel/
  collection.toml          name, shared defaults, shared variables
  environments/dev.toml    variables and secret bindings per target
  requests/**/*.toml       one request per file
  .runtime/                vault, sessions, captures, CA — gitignored
```

TOML rather than a bespoke format: it is already diffable, already reviewable in
a pull request, and costs no parser of our own to maintain.

## Design decisions worth knowing

**Requests execute natively, not in a page.** That is what lets a replay ignore
CORS and page CSP, speak HTTP/2, and present a client certificate.

**Secrets are referenced, never inlined.** `{{secret:name}}` resolves from the
vault at send time. The vault is opened lazily, so a request with no secrets in
it never makes macOS prompt for keychain access. Values that came out of the
vault are masked again on the way to the terminal.

**Cookie scoping is a security boundary.** Domain and path matching follow
RFC 6265 §5.1.3–5.1.4, including the dot boundary that stops `notbank.test`
from matching `bank.test`. There are tests for the leaks, not just the hits.

**The interception CA is per install.** It is generated on first use, written
`0600`, never shipped in a binary, and never installed into a trust store by us —
`gabriel ca` prints what to run and says to remove it afterwards. Hosts can be
excluded (`--exclude bank.test`) or interception narrowed (`--only api.test`);
anything out of scope is tunnelled byte-for-byte, unread.

## Building

```bash
cargo test          # 135 tests
cargo build --release
```

The keychain-backed vault test is `#[ignore]`d because it touches the real login
keychain; run it with `cargo test -p gabriel-vault -- --ignored`.

For CI or a container, set `GABRIEL_VAULT_PASSPHRASE` and the vault switches to
Argon2id key derivation instead of the keychain.

## Not built yet

Deliberately, and in the order the strategy document stages them: gRPC and SOAP,
mock servers and contract testing, the AI assistant, code generation, engine-level
content blocking, team sync, and the browser shell itself. The MVP is the loop
above; everything else waits until that loop is undeniably good.

Known gaps in what *is* built: bodies are buffered rather than streamed (so
server-sent events and websockets pass through but are not captured), OAuth2
covers the client-credentials and password grants but not the authorization-code
redirect flow, and the proxy speaks HTTP/1.1 to the browser.
