# Changelog

Notable changes, newest first. Versions follow [semver](https://semver.org);
before 1.0 the file format and CLI may still change between minor versions.

## v0.1.0-preview.2 — 2026-10-31

A security refresh of preview.1. **No functional change**: the CLI is still
frozen, no flag, file format or behaviour moved, and a collection written by
preview.1 works unchanged.

Ninety-one days passed between the two cuts. Nothing in the dependency tree
moved in that time; the advisory database did, and fourteen vulnerabilities
accumulated against versions that were current when preview.1 was built. Four
of them sit on surfaces this project makes claims about:

- **TLS session secrets could reach a debug output.** `rustls` `Debug` impls for
  client session values printed secret material. Gabriel asserts that no secret
  reaches any output surface and tests every one against canaries — but the
  canaries are Gabriel's own values, so a dependency printing TLS secrets is the
  one shape of that defect the suite cannot see. Fixed upstream in 0.23.46.
- **Certificate verification could accept distrusted intermediates.**
  `rustls-webpki` and both bundled root sets trusted intermediates that had been
  distrusted. Gabriel is an interception proxy, so upstream verification is what
  stands between a captured request and a forged one.
- **An unauthenticated `ServerHello` could downgrade later connections** to
  classical key exchange, and **Brotli certificate decompression could allocate
  up to 1 GiB before authentication**.
- **The vault's cipher crate had been yanked.** `chacha20` 0.10.1 — the vault is
  XChaCha20-Poly1305.

Also cleared: TLS 1.3 handshake messages accepted across encryption-level
boundaries, non-constant-time mask generation and `u128` comparisons in
`ctutils`, unbounded empty `DATA` frames in `h2`, and five `quinn-proto`
advisories covering zero-length frame panics, unbounded connection-ID
retirement and a bypassable reassembly guard.

Every fix was available within the versions the manifest already allowed, so no
dependency requirement changed — only the lock file. If you built preview.1 from
source yourself, `cargo update` gets you the same result.

### Why you did not hear about this from Gabriel

You did not, and you would not have. There is no telemetry, no update check and
no callback of any kind, which means a stale install stays stale silently. That
is the deliberate trade and this release is what the other side of it costs:
checking is your move to make, and `cargo audit` on the source tree is how.

## v0.1.0-preview.1 — 2026-07-31

First preview build. The loop works end to end and is tested against live
servers.

**Why "preview" and not "0.1.0".** Three things are not finished, and a version
number is a promise: OAuth is only verified against a local IdP, so
interoperability with Google, GitHub and Auth0 is unknown; there is no desktop
UI; and nothing here has been run by anyone outside the machine that wrote it.
The file format may change in response to what preview users hit. When those
answers exist, the release becomes 0.1.0.

### The loop

- **Capture proxy** — HTTP and HTTPS through a per-install MITM CA, with
  interception scoped by host (`--only`, `--exclude`). Streams event streams and
  oversized bodies straight through; relays WebSocket upgrades.
- **Promotion** — a captured request becomes a committable TOML file with no
  credentials in it: cookies become a session reference, bearer tokens move to
  the vault.
- **Replay** — carries the browser's session, so a promoted request stays
  authenticated without re-login.
- **Diff** — field-by-field comparison of two captured responses, ignoring the
  headers that change on every request.

### Protocols and auth

- REST, GraphQL request bodies, **server-sent events** (`run --stream`),
  **WebSocket** (`gabriel ws`).
- Bearer, Basic, API key, session cookies, **mTLS** client certificates.
- OAuth2: client credentials, password, and **authorization code with PKCE**
  (`gabriel auth`).

### Working with other tools

- **HAR import/export** — bring traffic in from DevTools, Charles or Proxyman;
  take it back out again. Export is complete by default; `--limit` truncates
  only when asked, and says so. Import tolerates what real exporters actually write:
  omitted `statusText` or `cache`, `null` in place of a measurement, timezone
  offsets, repeated `Set-Cookie` headers, and unreadable dates (whose file order
  is preserved). A structurally invalid entry fails the file rather than
  vanishing from it.
- **curl generation** — `gabriel curl <name>`, credentials masked by default.
- **JUnit and HTML reports** — `run --all --junit r.xml --html r.html`.
- **JWT inspection** — `gabriel jwt`, locally, with expiry and `alg: none`
  checks. Signatures are not verified.
- **Diagnostics** — `gabriel doctor` checks what breaks a first run and prints
  the fix for each. `gabriel feedback` gathers a support bundle you read and
  choose to share. There is no telemetry, and nothing is ever transmitted.

### Safety properties

- Secrets live in an XChaCha20-Poly1305 vault keyed from the OS keychain, or
  Argon2id where there is none. They are redacted from terminal output, reports
  and generated curl commands.
- Cookie scoping follows RFC 6265; credentials are dropped on a cross-origin
  redirect.
- Server-controlled text is escape-defused before printing, and escaped in
  reports.
- Credential-carrying headers (`Authorization`, `Cookie`, and relatives) are
  masked by name as well as by value, so a session cookie — which never passes
  through the vault — is not printed by `gabriel curl` or `--dry-run`.
- A reader that stops early — `gabriel ls | head -1`, `run --quiet | jq` — ends
  the command quietly instead of panicking, the way every other tool in a
  pipeline does.
- Replay is classified by effect: anything that changes state — including
  `PUT` and `DELETE`, whose first replay still lands — asks before running,
  `--dry-run` sends nothing, and `--yes` is the CI opt-in.
- Request paths cannot escape the collection, and what counts as an absolute
  path is the same answer on every platform rather than the host's opinion.
- Runtime files (vault, sessions, captures, CA key) are `0600` on Unix.
- The feedback bundle is assembled from an allow-list of safe fields, so the
  vault, session cookies, the CA key and captured traffic cannot reach it.
- **No secret leaves the process**, asserted the same way on every output
  surface: the terminal, JUnit XML, HTML reports, generated curl commands,
  error messages, panic messages, the support bundle and the vault file on
  disk. `gabriel har export` is the one deliberate exception, and there is a
  test saying so.

### Known limits

- WebSocket frames and streamed bodies pass through the proxy uncaptured.
- OAuth PKCE is verified against a local IdP; interoperability with Google,
  GitHub and Auth0 is untested. GitHub's OAuth apps ignore `code_verifier`.
- No desktop UI. This is a CLI, and it is feature-frozen: bug fixes only
  until the preview has answered what the workbench should be.
- Windows does not get the `0600` guarantee; files inherit directory ACLs.
