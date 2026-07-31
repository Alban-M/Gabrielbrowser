# Gabriel

**From real traffic to secure, repeatable API tests — in minutes.**

Gabriel records what your application actually sends, turns any of it into an
editable request with the credentials stripped out, and replays it still
authenticated. The same file then runs in CI.

```text
browse → capture → promote → replay (with the session intact) → diff
```

Other tools ask you to rebuild a request you have already made. This one starts
from the request itself — which is why the promotion step, not the protocol
list, is the thing worth having.

The [strategy document](docs/gabriel-browser-strategy.md) argues that the defensible
part of a "developer browser" is not the browser — it is the seam between the
browser, the API bench, and the proxy. This repository builds that seam, headless,
as the engine a desktop workbench would later sit on top of.

## Install

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/Alban-M/gabriel-releases/main/install.sh | sh
```

Binaries are published to a separate public repository,
[gabriel-releases](https://github.com/Alban-M/gabriel-releases) — this one holds
the source and stays private.

It works out the platform, verifies the SHA-256 before copying anything, clears
the macOS quarantine flag, and refuses to install a download it cannot verify.
`GABRIEL_VERSION` and `GABRIEL_INSTALL_DIR` override the defaults. On Windows,
take the `.zip` and put `gabriel.exe` on your PATH.

No release exists yet — the first tag has not been cut. Until then, from a
clone:

```bash
cargo install --path crates/gabriel-cli
```

Or straight from the repository, which needs access while it is private:

```bash
cargo install --git https://github.com/Alban-M/Gabrielbrowser gabriel-cli
```

### Verifying a download

Every release artifact carries a SHA-256 in `checksums.txt` and a keyless
Sigstore signature — the certificate binds the artifact to the workflow that
built it, so there is no signing key for anyone to steal:

```bash
sha256sum -c checksums.txt --ignore-missing

cosign verify-blob \
  --certificate gabriel-<target>.tar.gz.pem \
  --signature   gabriel-<target>.tar.gz.sig \
  --certificate-identity-regexp 'https://github.com/Alban-M/Gabrielbrowser/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  gabriel-<target>.tar.gz
```

A CycloneDX SBOM (`gabriel-sbom.cdx.json`) ships with every release,
listing every dependency in the shipped binary with its licence and package URL.

Binaries are not yet code-signed or notarized, so macOS will warn about an
unidentified developer.

Rust 1.85 or newer, for edition 2024.

## Five-minute quickstart

```bash
mkdir demo && cd demo
gabriel init                          # creates ./gabriel/
```

Record something real. In one terminal:

```bash
gabriel capture start                 # a proxy on 127.0.0.1:8888
```

Trust the CA once — `gabriel ca` prints the exact command for your platform —
then point a browser or `curl` at the proxy and use the site you are debugging:

```bash
curl --proxy http://127.0.0.1:8888 --cacert gabriel/.runtime/gabriel-ca.pem \
  https://httpbin.org/cookies/set?session_id=demo
```

Back in the first terminal, find the request and turn it into a file:

```bash
gabriel capture ls                    # newest first
gabriel promote <capture-id> --to users/me
```

The file it writes has no credentials in it. Replay it anyway:

```bash
gabriel run users/me                  # still authenticated
```

From here: `gabriel run --all --junit results.xml` in CI, `gabriel curl users/me`
to share a request, `gabriel jwt <token>` to inspect a token without pasting it
into a website, `gabriel ws <url>` for sockets, `gabriel har import <file>` to
bring in traffic recorded elsewhere.

## What works today

```bash
gabriel init                       # a collection is a folder of TOML files
gabriel capture start              # local proxy; every request is recorded
gabriel capture ls                 # find the request you care about
gabriel promote <capture-id>       # it becomes an editable, committable file
gabriel run <name>                 # replay it — still authenticated
gabriel diff <id-a> <id-b>         # what changed between two responses
gabriel run <name> --stream        # follow a server-sent event stream live
gabriel jwt <token>                # decode and inspect a JWT, locally
gabriel curl <name>                # print the request as a curl command
gabriel ws <url|name> --send '…'    # open a socket, send frames, watch replies
gabriel har import <file>          # bring in traffic from DevTools/Charles/Proxyman
gabriel har export --out t.har     # take it back out again
gabriel run --all --junit r.xml    # run the collection, report to CI
gabriel auth <name>                # OAuth2 sign-in (PKCE), tokens to the vault
gabriel doctor                     # check the environment before asking for help
gabriel feedback                   # bundle local diagnostics to read and maybe share
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
Credentials are also dropped when a redirect crosses origins, so an open
redirect cannot walk off with an `Authorization` header.

**Server-controlled text is defused before printing.** Response bodies, headers
and URLs are written by whoever is on the other end. Printed raw, an escape
sequence in any of them can erase Gabriel's output and replace it with something
convincing — so control bytes become caret notation (`^[`) when stdout is a
terminal. Pipes still receive the exact bytes, because `--quiet | jq` has to.

**No secret leaves the process, and every output surface proves it.**
Gabriel holds credentials on behalf of someone else, so anything that emits
text is somewhere one can escape. Each surface used to carry whatever assertion
its author thought of, which is how a surface added later ends up with none.
There is now one shared invariant: `gabriel-testkit` holds a set of canary
secrets and a single `assert_no_secret(surface, output)`, and the terminal,
JUnit XML, HTML reports, generated curl commands, error messages, the support
bundle and the vault file on disk are each asserted against it.

The canaries come in two shapes on purpose. Some are long and
credential-looking, which a pattern scrubber catches on its own; others are
short, lowercase and indistinguishable from prose, which only a surface that
*knows* which values are secret can redact. A surface that passes the first and
fails the second is pattern-matching where it should be remembering.

That includes the crash path: the panic hook is replaced with one that
scrubs before printing, using both the pattern scrubber and a registry of the
values actually resolved from the vault this run — because
`.expect(&format!("failed for {url}"))` is an ordinary thing to write and a
resolved URL can carry a token. Backtraces still print unscrubbed, since they
name functions and files rather than values.

`gabriel har export` is the one deliberate exception — a HAR is a faithful
record of traffic, which is what makes it interoperable and replayable — and
there is a test that says so, so it stays a decision rather than an oversight.

**Request paths cannot escape the collection.** `promote --to ../../elsewhere`
is refused rather than obeyed.

**The interception CA is per install.** It is generated on first use, written
`0600`, never shipped in a binary, and never installed into a trust store by us —
`gabriel ca` prints what to run and says to remove it afterwards. Hosts can be
excluded (`--exclude bank.test`) or interception narrowed (`--only api.test`);
anything out of scope is tunnelled byte-for-byte, unread.

## mTLS

A request can present a client certificate. The PEM must hold both the
certificate and its private key:

```toml
[settings]
# From the vault, so the key never sits on disk in the clear:
client_cert = "{{secret:client_identity}}"
# …or a path relative to the collection root, as `curl --cert` takes one:
# client_cert = "certs/client.pem"
```

## When something is wrong

```bash
gabriel doctor
```

Checks the things that actually break a first run — an untrusted or corrupt CA,
a proxy port already taken, a vault whose key is unreachable, credential files
with permissions that leak, an `HTTPS_PROXY` silently rerouting every request,
a collection that is not where you think. Each problem prints the fix. It needs
no network, creates nothing, and works outside a collection. `--json` for
scripts; exit code 1 only on a real failure, not a warning.

If that is not enough to explain the problem:

```bash
gabriel feedback
```

It writes a `gabriel-feedback/` directory — version, platform, `doctor` output,
your configuration with every value redacted, the last failures, and counts of
what the proxy captured. **Nothing is transmitted.** It runs only when you ask,
and the output is plain text you can read before deciding to attach it to a bug
report.

The bundle is built from a list of fields known to be safe rather than copied
and then stripped, so the vault, session cookies, the CA key, captured headers
and bodies, and the contents of your request files are absent by construction
rather than by filtering. Free text that does get in — error messages,
configuration values — is additionally scrubbed of URL passwords, JWTs, bearer
tokens, labelled keys and unlabelled credential-shaped strings. That scrubbing
is a second line of defence, not the first: read the files.

## Contributing

CI runs the suite on Linux, macOS and Windows, and gates on `cargo fmt --check`,
`cargo clippy -- -D warnings`, a dependency advisory scan, and the installer
regression suite:

```bash
cargo test
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
./installer-tests/run.sh        # needs cargo build --release first
./scripts/scan-secrets.sh README.md CHANGELOG.md docs/*.md
```

A new output surface is expected to bring a `no_secret_leaves_the_process`
module with it: feed the canaries from `gabriel-testkit` into every field the
surface renders, and assert on what comes out. Published prose has no canary to
inject, so `scan-secrets.sh` checks it by shape instead.

The installer has its own suite because it is a trust boundary — the one piece
of software someone runs before they have any reason to trust it, usually piped
straight into a shell. Both bugs it has had were invisible on the page and
obvious on the first execution.

## Building

```bash
cargo test          # 327 tests
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

Known gaps in what *is* built:

- **Streaming is delivered, not captured.** Event streams, `multipart/x-mixed-replace`,
  and bodies larger than the capture limit are passed straight through; the
  capture records their headers and status but not the body. Everything else is
  buffered so it can be captured in full.
- **Upgraded connections are relayed, not inspected.** A WebSocket handshake is
  forwarded verbatim and the sockets spliced, so the connection works and is
  recorded as a 101 — but the frames are not captured. The relay is exercised
  end to end over plain HTTP; the TLS path shares the same code but has only
  been verified by inspection.
- **OAuth2** covers client-credentials, password, and authorization-code with
  PKCE (`gabriel auth`). The PKCE flow is verified end to end against a local
  IdP that enforces the challenge; interoperability with Google, GitHub and
  Auth0 has **not** been tested — that needs real client IDs and a consent
  screen. GitHub's OAuth apps are not expected to work, as they ignore
  `code_verifier`.
- **JWT signatures are not verified** — that needs the issuer's key. Gabriel
  decodes, checks expiry, and flags `alg: none`.
- **The proxy speaks HTTP/1.1** to the browser.
- **`gabriel curl` emits POSIX quoting.** Paste it into bash, zsh or WSL. It is
  not correct for `cmd.exe` or PowerShell, whose quoting rules differ.
- **Looking up an old capture is a scan.** Reads walk backwards from the end of
  the log, so the newest captures are instant and the oldest in a large log is
  not — see [docs/performance.md](docs/performance.md).
