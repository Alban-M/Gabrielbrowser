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

[Where this is going](docs/vision.md) · [how it should look](docs/design.md) ·
[the signature screen, built](docs/mockup/workbench.html).

The [strategy document](docs/gabriel-browser-strategy.md) argues that the defensible
part of a "developer browser" is not the browser — it is the seam between the
browser, the API bench, and the proxy. This repository builds that seam, headless,
as the engine a desktop workbench would later sit on top of.

## Install

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/Alban-M/gabriel-releases/main/install.sh | sh
```

Binaries are published to a separate repository,
[gabriel-releases](https://github.com/Alban-M/gabriel-releases), so that
installing needs nothing from here. This one holds the source.

It works out the platform, verifies the SHA-256 before copying anything, clears
the macOS quarantine flag, and refuses to install a download it cannot verify.
`GABRIEL_VERSION` and `GABRIEL_INSTALL_DIR` override the defaults. On Windows,
take the `.zip` and put `gabriel.exe` on your PATH.

No release exists yet — the first tag has not been cut. Until then, from a
clone:

```bash
cargo install --path crates/gabriel-cli
```

Or straight from the repository:

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

The decisions below share a shape, and it is worth naming because it decides
arguments quickly. Each is an answer to "how is this enforced", and the answers
form a ladder — every rung is strictly stronger than the one beneath it:

1. **Documentation.** The weakest. It moves the work of noticing onto the person
   least able to do it, and it is thinnest exactly where it matters most —
   the scripted run with stderr discarded.
2. **A safe default.** Danger requires an explicit act. Allow-list construction
   for the support bundle; a complete export unless `--limit` is given.
3. **Binding.** The thing that was checked is the thing that gets used: verify
   the checksum, then install *those bytes*. This rung exists because the gap
   between "what was verified" and "what was used" is where the worst bugs live
   — a failure here happens *after* a check passed, so it manufactures
   confidence rather than merely permitting harm. Redaction that ran, showed a
   mask, and left the credential in place is this failure exactly.
4. **Structural impossibility.** The invalid state cannot be represented. A
   `Removal` record with no field for the value it removed cannot leak it,
   whoever writes the next line of code.

Prefer the highest rung the problem allows. Most of the fixes in this codebase
were a move up it, not a new feature.

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

Three further invariants are written down but not yet needed: **no secret
leaves the machine without explicit user approval**, **the user can always
determine why a piece of information left the machine**, and **every
information-flow decision is reproducible.** Nothing today violates them — there is
no telemetry, no update check and no crash reporting, and every host in the
shipped code is a test domain or loopback. It exists because the AI layer will
turn redaction from a property of output into a property of *input*, and that
boundary is much cheaper to design than to retrofit. See
[docs/information-flow.md](docs/information-flow.md).

**A persisted artifact is complete by default; only a view may be partial.**
`gabriel capture ls` shows the newest 30 and that is obviously partial — the
screen is right there. A file is not: one holding a fraction of the log is
indistinguishable from a whole one, and the person who reads it later is not the
person who ran the command. So exports and reports are complete unless
truncation was asked for, and a bundle that caps something says so in its own
contents. `har export` once defaulted to the newest 1000 with a warning on
stderr, which failed exactly where it mattered — a scripted export in CI, stderr
discarded, producing a file someone later trusted. A warning is not a substitute
for a safe default, because it moves the work of noticing onto the person least
able to do it.

**An error message distinguishes what was observed from what might have caused
it.** A message is a claim about reality, and a confident wrong one is worse
than no message: the reader reasons correctly from a false premise, which takes
much longer to recover from than knowing nothing. So state the observation,
then list causes without picking one.

```text
no:   Repository is private. Make it public.
yes:  Could not access the repository.
        <git's own error>
      Possible causes: it does not exist, this machine cannot authenticate
      to it, or permissions are insufficient. Check the message above.
```

Both halves of that example are real. The repository was public; the actual
fault was a missing SSH key, and the message would have sent someone to change
a setting that was already correct.

This is a rule rather than an invariant, and the distinction is honest: the four
information-flow invariants have `gabriel-testkit` behind them, while this one
rests on review. "Does this message assert something the code did not check" is
not mechanically decidable, so it is enforced by whoever reads the diff.

**Replay is bounded by what it does to the world.** Replaying a `GET` is free;
replaying a payment charges the card twice. Gabriel classifies every request by
RFC 9110 method semantics and asks before running anything that changes state.

The axis is *does this alter the target*, not *is it repeatable* — because
idempotence describes the **second** call. The first replay of
`DELETE /customers/123` still deletes the customer, and the first `PUT` still
overwrites production config. So only a read runs unannounced; `PUT` and
`DELETE` are announced as stable-on-repeat but landing the first time, and
`POST`/`PATCH` as performing the action again. Gabriel refuses rather than
guessing where there is no terminal to ask at. `--dry-run` resolves
everything and sends nothing. `--yes` is the explicit opt-in, which is the right
shape for CI: a config that says `--yes` is a reviewable decision, whereas a
prompt nobody can answer is a hung build.

`run --all` skips what it cannot confirm and says how many, because failing a
whole collection run over one `POST` would push every CI config to a blanket
`--yes` and defeat the point. Method is a good guess and wrong in one common
direction — a `POST` used for search — so `[settings] effect = "read"` lets the
author say *I checked*. Promotion never writes it: a capture cannot know whether
the endpoint it saw was a search or a purchase.

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

## The CLI is frozen

As of `v0.1.0-preview.1` the CLI takes **bug fixes only**: CI failures, OAuth
interoperability defects, security defects, installer and release defects, and
anything that stops someone reaching a first successful replay.

Everything else — features, ergonomics, output formatting — goes to the desktop
workbench backlog instead, with the observation that produced it attached. The
engine is not short of capability; what it is short of is evidence about how
people use it. See [docs/preview-1.md](docs/preview-1.md) for what the preview
is measuring and [docs/oauth-interop.md](docs/oauth-interop.md) for the one
piece of engineering validation still outstanding.

## Contributing

**A local pass is not a cross-platform pass.** `cargo test` green on one machine
means that machine can build Gabriel; it says nothing about the others. At the
time of writing CI confirms Linux and macOS green and **Windows failing** at the
test step — a real defect, not a flake, and a release blocker. The distinction is
worth keeping in mind whenever this README claims a number.

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

**A tagged commit must contain the machinery required to publish itself.** A
tag-triggered workflow runs the definition stored *at that tag*, so a release
workflow fixed afterwards on `main` does not help the tag that needs it — and
re-running the failed run replays the same broken definition. When the first
release run failed on a bug in its own smoke job, the fix was to re-cut the tag
at the commit carrying the repair, not to re-run the old one. The release
workflow is part of the release, not scaffolding beside it.

Before a release, walk [docs/manual-test.md](docs/manual-test.md) — ten minutes,
no network needed, and it exercises the loop the automated suite can only test
in pieces.

A new output surface is expected to bring a `no_secret_leaves_the_process`
module with it: feed the canaries from `gabriel-testkit` into every field the
surface renders, and assert on what comes out. Published prose has no canary to
inject, so `scan-secrets.sh` checks it by shape instead.

**When a claim concerns runtime behaviour and the behaviour is inexpensive to
observe, prefer an experiment over inference.** Reading the code is how you form
the hypothesis. It is not how you settle it.

The claims that have been wrong here were wrong in a consistent way: each was
about behaviour, each took under a minute to settle by running something, and
each had already been argued for longer than that. `cwd` was "unsupported" in
`launch.json` until pointing it outside the project produced a validation error
saying otherwise. `har export` was "complete" until a 100,000-capture log
produced a file with 1,000 in it. The mockup "rendered correctly" — over
`file://`, where nothing revealed that serving it over HTTP mangled every em
dash.

The condition in the first sentence is doing real work, so do not drop it.
Verifying OAuth against Google or Auth0 needs client IDs and a human at a
consent screen; that experiment is expensive, which is why
[the runbook](docs/oauth-interop.md) exists instead and why the interop table
in it is honestly empty. Inference is the right tool when observation is dear.
It is the wrong one when observation costs thirty seconds.

The installer has its own suite because it is a trust boundary — the one piece
of software someone runs before they have any reason to trust it, usually piped
straight into a shell. Both bugs it has had were invisible on the page and
obvious on the first execution.

## Building

```bash
cargo test          # 394 tests
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
  A consequence worth knowing, and currently only documented rather than
  enforced: a capture records an absent body the same way whether the body was
  genuinely empty or was never captured, so a HAR export cannot tell a consumer
  which it was. Declaring it in the artifact needs the distinction recorded at
  capture time — a change to the capture format, which the freeze excludes. This
  is the weakest available rung, chosen because the stronger ones are not
  available yet, not because it is sufficient.
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
