# OAuth PKCE interoperability runbook

The authorization-code + PKCE flow is verified end to end against a local
identity provider that enforces the challenge. It has **never been run against
a real provider**, because that needs a client ID and a human at a consent
screen. This is the procedure for closing that gap, written so it is mechanical
once credentials exist.

Every configuration below was run against the real binary and produced a
correct authorization URL — right `code_challenge`, `code_challenge_method=S256`,
`state`, and percent-encoded `redirect_uri`. What is unverified is what the
*provider* does with them.

## Before you start

```sh
export GABRIEL_VAULT_PASSPHRASE=…    # or let it use the OS keychain
gabriel init oauth-interop && cd oauth-interop
```

Register `http://127.0.0.1:8765/callback` as an allowed redirect URI with each
provider. The fixed port matters: providers require an exact match, and
`--port 8765` makes Gabriel bind that port instead of taking a free one.

## Google

Create an OAuth client of type **Web application** in the Google Cloud console.
Google issues no client secret requirement for PKCE public clients, but a Web
application client does get one — leave `client_secret` out and see whether the
token exchange is accepted without it. That is one of the things being tested.

`gabriel/requests/google/me.toml`:

```toml
name = "Google userinfo"
method = "GET"
url = "https://openidconnect.googleapis.com/v1/userinfo"

[auth]
type = "oauth2"
grant = "authorization_code"
authorize_url = "https://accounts.google.com/o/oauth2/v2/auth"
token_url = "https://oauth2.googleapis.com/token"
client_id = "REPLACE.apps.googleusercontent.com"
redirect_uri = "http://127.0.0.1:8765/callback"
scope = "openid email profile"
```

```sh
gabriel auth google/me --port 8765
gabriel run google/me
```

**Pass:** the browser returns, `gabriel auth` reports tokens stored, and
`gabriel run google/me` returns 200 with your profile.

## Auth0

Create a **Native** application (not Single Page or Regular Web) — native is
the type Auth0 treats as a public PKCE client.

`gabriel/requests/auth0/me.toml`:

```toml
name = "Auth0 userinfo"
method = "GET"
url = "https://YOUR-TENANT.eu.auth0.com/userinfo"

[auth]
type = "oauth2"
grant = "authorization_code"
authorize_url = "https://YOUR-TENANT.eu.auth0.com/authorize"
token_url = "https://YOUR-TENANT.eu.auth0.com/oauth/token"
client_id = "REPLACE_CLIENT_ID"
redirect_uri = "http://127.0.0.1:8765/callback"
scope = "openid profile email"
audience = "https://YOUR-TENANT.eu.auth0.com/api/v2/"
```

`audience` is Auth0-specific and is verified to reach the authorize URL. Without
it Auth0 issues an opaque token rather than a JWT, and `/userinfo` still works —
so if you want to exercise `gabriel jwt` on the result, keep it.

```sh
gabriel auth auth0/me --port 8765
gabriel run auth0/me
gabriel jwt "$(gabriel vault get auth0-access-token 2>/dev/null)"   # optional
```

## GitHub

Expected to work while **ignoring PKCE entirely** — GitHub's OAuth apps do not
implement `code_verifier`, so the challenge is sent and disregarded. Worth
running anyway: the flow should still complete, which proves Gabriel does not
depend on the provider echoing anything back.

```toml
name = "GitHub viewer"
method = "GET"
url = "https://api.github.com/user"

[auth]
type = "oauth2"
grant = "authorization_code"
authorize_url = "https://github.com/login/oauth/authorize"
token_url = "https://github.com/login/oauth/access_token"
client_id = "REPLACE_CLIENT_ID"
redirect_uri = "http://127.0.0.1:8765/callback"
scope = "read:user"
```

GitHub's token endpoint returns `application/x-www-form-urlencoded` unless asked
for JSON. If the exchange fails with a parse error, that is the finding — note
it rather than working around it locally.

## What to record for each provider

Run `gabriel auth <name> --no-browser --port 8765` first and keep the printed
URL. It is the exact request being made, and it is what a provider's support
will ask for.

| Question | How you know |
| --- | --- |
| Did the authorize step succeed? | The browser reached a consent screen rather than an error page |
| Did the redirect come back? | `gabriel auth` printed that tokens were stored |
| Was PKCE actually enforced? | Re-run with a deliberately wrong verifier (below) |
| Did the token work? | `gabriel run <name>` returned 2xx |
| Does refresh work? | Wait for expiry, or shorten it, then `gabriel run` again |

### Confirming the provider enforces PKCE

A provider that ignores `code_verifier` accepts an exchange that should fail.
To tell "PKCE works" from "PKCE is ignored", complete the flow while watching
the token exchange through Gabriel's own proxy:

```sh
gabriel capture start --only accounts.google.com --only oauth2.googleapis.com
```

Then check the captured token request contains `code_verifier`, and that the
provider rejected a tampered one. If the provider accepts a wrong verifier, PKCE
is decorative there — record it as a provider limitation, not a Gabriel defect.

## If something fails

Capture the failure with the flow's own tooling rather than describing it:

```sh
gabriel feedback
```

The bundle redacts values but keeps the shape, which is what a provider bug
looks like. Note which of the five rows above failed, and the exact error the
provider returned — provider error strings are the actionable part.

## Status

| Provider | Authorize | Token exchange | Refresh | PKCE enforced |
| --- | --- | --- | --- | --- |
| Local test IdP | ✅ | ✅ | ✅ | ✅ |
| Google | — | — | — | — |
| Auth0 | — | — | — | — |
| GitHub | — | — | — | not implemented by GitHub |

Fill this in as it is run; it is the gate the desktop workbench waits on.
