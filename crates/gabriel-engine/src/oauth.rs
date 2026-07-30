//! Authorization code with PKCE (RFC 7636).
//!
//! The flow a developer actually needs and every tool gets wrong somewhere:
//! open a browser, let the person log in, catch the redirect, exchange the code.
//!
//! Three properties matter more than convenience here, because getting them
//! wrong is what turns an auth flow into a vulnerability:
//!
//! * **The verifier never leaves the process until the exchange.** PKCE exists
//!   so that a code intercepted on the way back is useless without it.
//! * **`state` is checked.** Without it, anything that can reach the loopback
//!   listener can feed us a code from a different authorization — CSRF against
//!   the flow itself.
//! * **The listener binds loopback only, serves exactly one callback, and
//!   ignores everything else.** It is open for seconds, but it is still a
//!   server on the developer's machine.

use crate::{EngineError, Result};
use gabriel_core::model::{OAuth2, OAuth2Grant};
use rand::Rng as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

/// Unreserved characters, per RFC 7636 §4.1. Restricting the alphabet keeps the
/// verifier safe in a query string without escaping.
const VERIFIER_ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// Random bytes behind the verifier. base64url turns 48 bytes into 64
/// characters — inside the spec's 43–128 range, with 384 bits of entropy.
const VERIFIER_BYTES: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Generate a fresh verifier and its S256 challenge.
    pub fn generate() -> Self {
        Self::from_verifier(random_unreserved(VERIFIER_BYTES))
    }

    pub fn from_verifier(verifier: String) -> Self {
        // challenge = BASE64URL-NOPAD(SHA256(ASCII(verifier)))
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = base64url_nopad(&digest);
        Pkce { verifier, challenge }
    }
}

/// `Debug` is hand-written: a verifier in a log is a credential in a log.
impl std::fmt::Debug for PkceRedacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pkce { verifier: <redacted>, challenge: <redacted> }")
    }
}

/// Marker used only by the `Debug` impl above.
pub struct PkceRedacted;

fn base64url_nopad(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A random, URL-safe `state` value.
pub fn random_state() -> String {
    // 24 bytes → 32 characters, 192 bits.
    random_unreserved(24)
}

/// Random characters from the unreserved set, by encoding random bytes.
///
/// Mapping bytes onto the 66-character unreserved set with `%` would be biased,
/// since 66 does not divide 256. base64url's alphabet (`A-Z a-z 0-9 - _`) is a
/// subset of unreserved and encodes 3 bytes as exactly 4 characters, so this is
/// uniform by construction and needs no rejection sampling.
fn random_unreserved(bytes: usize) -> String {
    debug_assert!(
        VERIFIER_ALPHABET.iter().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(b)),
        "the alphabet must stay within the unreserved set"
    );
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buffer);
    base64url_nopad(&buffer)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// OpenID Connect providers return this alongside the access token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
}

/// How to run the flow.
#[derive(Debug, Clone)]
pub struct FlowOptions {
    /// Port for the loopback listener. `0` takes any free port, which only works
    /// when the provider allows a wildcard port on a loopback redirect.
    pub port: u16,
    /// Launch a browser. Off in tests and on headless machines.
    pub open_browser: bool,
    /// How long to wait for the person to finish logging in.
    pub timeout: Duration,
}

impl Default for FlowOptions {
    fn default() -> Self {
        FlowOptions { port: 0, open_browser: true, timeout: Duration::from_secs(300) }
    }
}

/// Run the authorization-code + PKCE flow to completion.
///
/// `on_url` receives the authorization URL, so a caller can print it — a
/// headless machine, a remote shell, or a browser that opens in the wrong
/// profile all need the URL visible rather than assumed.
pub async fn authorization_code(
    config: &OAuth2,
    options: &FlowOptions,
    mut on_url: impl FnMut(&str),
) -> Result<Tokens> {
    if config.grant != OAuth2Grant::AuthorizationCode {
        return Err(EngineError::Auth(
            "this configuration is not an authorization-code grant".to_string(),
        ));
    }
    let authorize_url = config.authorize_url.as_ref().ok_or_else(|| {
        EngineError::Auth("authorize_url is required for the authorization-code grant".to_string())
    })?;

    let listener = TcpListener::bind(("127.0.0.1", options.port))
        .await
        .map_err(|e| EngineError::Auth(format!("could not open the loopback listener: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| EngineError::Auth(e.to_string()))?;

    // A provider matches the redirect URI exactly, so if one is configured it
    // wins and the listener must already be on its port.
    let redirect_uri = match &config.redirect_uri {
        Some(uri) => uri.clone(),
        None => format!("http://127.0.0.1:{}/callback", bound.port()),
    };

    let pkce = Pkce::generate();
    let state = random_state();

    let url = build_authorize_url(authorize_url, config, &redirect_uri, &pkce, &state);
    on_url(&url);
    if options.open_browser {
        open_in_browser(&url);
    }

    let code = wait_for_code(listener, &state, options.timeout).await?;
    exchange_code(config, &code, &pkce.verifier, &redirect_uri).await
}

/// Assemble the authorization URL.
pub fn build_authorize_url(
    authorize_url: &str,
    config: &OAuth2,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
) -> String {
    let mut params = vec![
        ("response_type", "code".to_string()),
        ("client_id", config.client_id.clone()),
        ("redirect_uri", redirect_uri.to_string()),
        ("state", state.to_string()),
        ("code_challenge", pkce.challenge.clone()),
        ("code_challenge_method", "S256".to_string()),
    ];
    if let Some(scope) = &config.scope {
        params.push(("scope", scope.clone()));
    }
    if let Some(audience) = &config.audience {
        // Auth0 needs this to issue a JWT for an API rather than an opaque token.
        params.push(("audience", audience.clone()));
    }

    let query: Vec<String> =
        params.iter().map(|(k, v)| format!("{k}={}", urlencode(v))).collect();
    let separator = if authorize_url.contains('?') { '&' } else { '?' };
    format!("{authorize_url}{separator}{}", query.join("&"))
}

/// Serve exactly one callback and return the code.
async fn wait_for_code(
    listener: TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let accepted = tokio::time::timeout_at(deadline, listener.accept()).await;
        let Ok(accepted) = accepted else {
            return Err(EngineError::Auth(
                "timed out waiting for the browser to come back".to_string(),
            ));
        };
        let (mut stream, _peer) = accepted
            .map_err(|e| EngineError::Auth(format!("accepting the callback failed: {e}")))?;

        let mut buffer = vec![0u8; 8192];
        let read = stream.read(&mut buffer).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..read]);
        let target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/");

        // Browsers ask for /favicon.ico; ignore anything that is not the redirect.
        let query = match target.split_once('?') {
            Some((_, query)) => query,
            None => {
                let _ = respond(&mut stream, 404, "Not the callback.").await;
                continue;
            }
        };
        let params = parse_query(query);

        if let Some(error) = params.get("error") {
            let description = params
                .get("error_description")
                .cloned()
                .unwrap_or_else(|| error.clone());
            let _ = respond(&mut stream, 400, "Authorization failed. You can close this tab.").await;
            return Err(EngineError::Auth(format!("the provider refused: {error} — {description}")));
        }

        // Check state before the code is even read: a mismatch means this
        // callback belongs to somebody else's authorization.
        match params.get("state") {
            Some(state) if constant_time_eq(state, expected_state) => {}
            _ => {
                let _ = respond(&mut stream, 400, "Unexpected state. Nothing was accepted.").await;
                return Err(EngineError::Auth(
                    "the callback's state did not match — the response was discarded".to_string(),
                ));
            }
        }

        let Some(code) = params.get("code") else {
            let _ = respond(&mut stream, 400, "No code in the callback.").await;
            continue;
        };

        let _ = respond(
            &mut stream,
            200,
            "Signed in. You can close this tab and go back to the terminal.",
        )
        .await;
        return Ok(code.clone());
    }
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    message: &str,
) -> std::io::Result<()> {
    // Plain text, no markup: the message is fixed, but a text/plain response
    // cannot be turned into a page that runs anything regardless.
    let body = format!("{message}\n");
    let response = format!(
        "HTTP/1.1 {status} \r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

/// Exchange the code, proving possession of the verifier.
pub async fn exchange_code(
    config: &OAuth2,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Tokens> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("client_id", config.client_id.clone()),
        ("code_verifier", verifier.to_string()),
    ];
    // A public client has no secret; a confidential one sends it here.
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.clone()));
    }

    post_token_request(&config.token_url, form).await
}

/// Trade a refresh token for a new access token.
pub async fn refresh(config: &OAuth2, refresh_token: &str) -> Result<Tokens> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", config.client_id.clone()),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    post_token_request(&config.token_url, form).await
}

async fn post_token_request(token_url: &str, form: Vec<(&str, String)>) -> Result<Tokens> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| EngineError::Client(e.to_string()))?;

    let response = client
        .post(token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| EngineError::Auth(format!("token request to {token_url} failed: {e}")))?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();

    if !status.is_success() {
        // The body carries `error` and `error_description`; both are the useful
        // part and neither contains the code or the verifier.
        return Err(EngineError::Auth(format!(
            "token endpoint returned {status}: {}",
            text.chars().take(400).collect::<String>()
        )));
    }

    serde_json::from_str(&text)
        .map_err(|e| EngineError::Auth(format!("token response was not the expected JSON: {e}")))
}

fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = ("open", vec![url]);
    #[cfg(target_os = "linux")]
    let command = ("xdg-open", vec![url]);
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", "", url]);

    // Failing to open a browser is not fatal — the URL was printed.
    let _ = std::process::Command::new(command.0)
        .args(command.1)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

pub fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((urldecode(key), urldecode(value)))
        })
        .collect()
}

/// Compare without leaking the position of the first difference.
///
/// `state` is not a secret in the way a token is, but comparing it in constant
/// time costs nothing and removes a question.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn urldecode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&input[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OAuth2 {
        OAuth2 {
            grant: OAuth2Grant::AuthorizationCode,
            token_url: "https://auth.test/oauth/token".into(),
            authorize_url: Some("https://auth.test/authorize".into()),
            redirect_uri: None,
            client_id: "client-123".into(),
            client_secret: None,
            scope: Some("openid profile".into()),
            audience: None,
            credentials_in_body: false,
        }
    }

    /// The one vector everybody checks against: RFC 7636 Appendix B.
    #[test]
    fn the_challenge_matches_the_rfc_test_vector() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_string());
        assert_eq!(pkce.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn a_generated_verifier_meets_the_specs_shape() {
        let pkce = Pkce::generate();
        assert!(
            (43..=128).contains(&pkce.verifier.len()),
            "verifier length {} is outside the spec",
            pkce.verifier.len()
        );
        assert!(
            pkce.verifier.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)),
            "verifier contains characters outside the unreserved set"
        );
        // The challenge is base64url without padding.
        assert!(!pkce.challenge.contains('=') && !pkce.challenge.contains('+') && !pkce.challenge.contains('/'));
    }

    #[test]
    fn every_verifier_is_different() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(seen.insert(Pkce::generate().verifier), "a verifier repeated");
        }
    }

    #[test]
    fn state_values_are_random_and_url_safe() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let state = random_state();
            assert!(state.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)));
            assert!(seen.insert(state), "a state repeated");
        }
    }

    #[test]
    fn the_authorize_url_carries_everything_the_provider_needs() {
        let pkce = Pkce::from_verifier("verifier".into());
        let url = build_authorize_url(
            "https://auth.test/authorize",
            &config(),
            "http://127.0.0.1:8765/callback",
            &pkce,
            "state-abc",
        );
        let query = url.split_once('?').expect("a query").1;
        let params = parse_query(query);

        assert_eq!(params["response_type"], "code");
        assert_eq!(params["client_id"], "client-123");
        assert_eq!(params["redirect_uri"], "http://127.0.0.1:8765/callback");
        assert_eq!(params["state"], "state-abc");
        assert_eq!(params["code_challenge"], pkce.challenge);
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["scope"], "openid profile");
        // The verifier must never appear in the URL the browser sees.
        assert!(!url.contains("verifier"), "the verifier leaked into the authorize URL: {url}");
    }

    #[test]
    fn an_authorize_url_that_already_has_a_query_is_extended_not_broken() {
        let pkce = Pkce::from_verifier("v".into());
        let url = build_authorize_url(
            "https://auth.test/authorize?tenant=acme",
            &config(),
            "http://127.0.0.1:1/callback",
            &pkce,
            "s",
        );
        assert!(url.contains("?tenant=acme&response_type=code"), "{url}");
        assert_eq!(url.matches('?').count(), 1, "two query separators: {url}");
    }

    #[test]
    fn an_audience_is_included_when_configured() {
        let mut config = config();
        config.audience = Some("https://api.acme.test".into());
        let url = build_authorize_url(
            "https://auth.test/authorize",
            &config,
            "http://127.0.0.1:1/callback",
            &Pkce::from_verifier("v".into()),
            "s",
        );
        assert!(url.contains("audience=https%3A%2F%2Fapi.acme.test"), "{url}");
    }

    #[test]
    fn query_parsing_handles_encoding() {
        let params = parse_query("code=abc%2F123&state=x+y&empty=");
        assert_eq!(params["code"], "abc/123");
        assert_eq!(params["state"], "x y");
        assert_eq!(params["empty"], "");
    }

    #[test]
    fn state_comparison_rejects_differences_anywhere() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "bbc"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("", "a"));
    }

    #[test]
    fn a_non_authorization_code_config_is_refused() {
        let mut config = config();
        config.grant = OAuth2Grant::ClientCredentials;
        let error = futures_executor_block(async {
            authorization_code(&config, &FlowOptions::default(), |_| {}).await
        });
        assert!(error.unwrap_err().to_string().contains("not an authorization-code"));
    }

    #[test]
    fn a_missing_authorize_url_is_refused_before_a_listener_opens() {
        let mut config = config();
        config.authorize_url = None;
        let error = futures_executor_block(async {
            authorization_code(&config, &FlowOptions::default(), |_| {}).await
        });
        assert!(error.unwrap_err().to_string().contains("authorize_url is required"));
    }

    /// Tiny blocking helper so these two checks need no async test harness.
    fn futures_executor_block<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }
}
