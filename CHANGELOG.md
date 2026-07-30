# Changelog

Notable changes, newest first. Versions follow [semver](https://semver.org);
before 1.0 the file format and CLI may still change between minor versions.

## v0.1.0 — unreleased

First public preview. The loop works end to end and is tested against live
servers; the file format is not yet frozen.

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
  take it back out again.
- **curl generation** — `gabriel curl <name>`, credentials masked by default.
- **JUnit and HTML reports** — `run --all --junit r.xml --html r.html`.
- **JWT inspection** — `gabriel jwt`, locally, with expiry and `alg: none`
  checks. Signatures are not verified.

### Safety properties

- Secrets live in an XChaCha20-Poly1305 vault keyed from the OS keychain, or
  Argon2id where there is none. They are redacted from terminal output, reports
  and generated curl commands.
- Cookie scoping follows RFC 6265; credentials are dropped on a cross-origin
  redirect.
- Server-controlled text is escape-defused before printing, and escaped in
  reports.
- Request paths cannot escape the collection.
- Runtime files (vault, sessions, captures, CA key) are `0600` on Unix.

### Known limits

- WebSocket frames and streamed bodies pass through the proxy uncaptured.
- OAuth PKCE is verified against a local IdP; interoperability with Google,
  GitHub and Auth0 is untested. GitHub's OAuth apps ignore `code_verifier`.
- No desktop UI. This is a CLI.
- Windows does not get the `0600` guarantee; files inherit directory ACLs.
