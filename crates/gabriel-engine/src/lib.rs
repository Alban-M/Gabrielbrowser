//! The request execution engine.
//!
//! Requests run here, in a native HTTP client — not in a page's JavaScript
//! context. That is what lets a replayed request ignore CORS and page CSP,
//! speak HTTP/2, present a client certificate, and carry a session the
//! *browser* established. An API bench living inside the page could do none of
//! it; one living beside the browser can.

pub mod assertion;
pub mod session;

use assertion::AssertionOutcome;
use gabriel_core::model::{ApiKeyLocation, Auth, Body, CaptureSource, OAuth2, OAuth2Grant, RequestSpec};
use gabriel_core::response::{ExecutedResponse, Timings};
use gabriel_core::vars::Resolver;
use gabriel_core::{Error as CoreError, jsonpath};
use session::{DEFAULT_SESSION, SessionStore};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Template(#[from] CoreError),

    #[error("{url} is not a valid URL: {message}")]
    BadUrl { url: String, message: String },

    #[error("request failed: {0}")]
    Transport(String),

    #[error("could not build an HTTP client: {0}")]
    Client(String),

    #[error("{0}")]
    Auth(String),

    #[error("body file {path}")]
    BodyFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("client certificate {path}")]
    ClientCertFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("client certificate `{reference}` is not a usable PEM identity: {message}")]
    ClientCert { reference: String, message: String },
}

type Result<T> = std::result::Result<T, EngineError>;

/// What a request needs to know about the world outside its own file.
pub struct RunContext<'r, 'v> {
    pub resolver: &'r mut Resolver<'v>,
    pub sessions: &'r mut SessionStore,
    /// Session used when a request says `auth = "session"` without naming one.
    pub session: String,
    /// Whether `Set-Cookie` from the response updates the session store. On for
    /// interactive runs; a caller replaying a fixture may want it off.
    pub record_cookies: bool,
    /// Root for relative body-file paths.
    pub base_dir: PathBuf,
}

impl<'r, 'v> RunContext<'r, 'v> {
    pub fn new(resolver: &'r mut Resolver<'v>, sessions: &'r mut SessionStore) -> Self {
        RunContext {
            resolver,
            sessions,
            session: DEFAULT_SESSION.to_string(),
            record_cookies: true,
            base_dir: PathBuf::from("."),
        }
    }

    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = session.into();
        self
    }

    pub fn with_base_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.base_dir = dir.into();
        self
    }
}

/// Everything that happened when one request ran.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// The request as actually sent, after template resolution — this is what a
    /// developer needs to see when the answer is "you sent the wrong thing".
    pub sent: SentRequest,
    pub response: ExecutedResponse,
    pub assertions: Vec<AssertionOutcome>,
    /// Variables bound from the response, in declaration order.
    pub captured: Vec<(String, String)>,
    /// Redirects followed to reach the final response, in order.
    pub redirects: Vec<Hop>,
}

impl RunOutcome {
    pub fn assertions_passed(&self) -> bool {
        self.assertions.iter().all(|a| a.passed)
    }

    pub fn failed_assertions(&self) -> impl Iterator<Item = &AssertionOutcome> {
        self.assertions.iter().filter(|a| !a.passed)
    }
}

#[derive(Debug, Clone)]
pub struct SentRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// One redirect the engine followed. Worth surfacing: "the redirects happen
/// invisibly" is the reason OAuth flows are painful to debug.
#[derive(Debug, Clone, PartialEq)]
pub struct Hop {
    pub status: u16,
    /// The URL that produced the redirect.
    pub url: String,
    /// Where it pointed.
    pub location: String,
}

/// Jar name for cookies collected within a single redirect chain.
const CHAIN_SESSION: &str = "\0chain";

/// Combine two `Cookie` header values, keeping one entry per cookie name.
/// `fresher` wins on conflict.
fn merge_cookie_headers(base: Option<&str>, fresher: Option<&str>) -> Option<String> {
    match (base, fresher) {
        (None, None) => None,
        (Some(only), None) | (None, Some(only)) => Some(only.to_string()),
        (Some(base), Some(fresher)) => {
            let name_of = |pair: &str| {
                pair.split_once('=').map(|(n, _)| n.trim().to_string()).unwrap_or_default()
            };
            let overridden: Vec<String> = fresher.split("; ").map(name_of).collect();
            let mut merged: Vec<&str> = fresher.split("; ").collect();
            for pair in base.split("; ") {
                if !overridden.contains(&name_of(pair)) {
                    merged.push(pair);
                }
            }
            Some(merged.join("; "))
        }
    }
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
}

/// Headers that must not survive a cross-origin redirect.
fn strip_credentials(headers: &mut Vec<(String, String)>) {
    const SENSITIVE: &[&str] = &["authorization", "cookie", "proxy-authorization", "www-authenticate"];
    headers.retain(|(name, _)| !SENSITIVE.contains(&name.to_ascii_lowercase().as_str()));
}

/// Reusable HTTP clients and cached OAuth tokens.
///
/// Clients are pooled by their settings so that a collection run reuses
/// connections instead of paying a TLS handshake per request.
#[derive(Default)]
pub struct Executor {
    clients: HashMap<ClientKey, reqwest::Client>,
    oauth_tokens: HashMap<String, CachedToken>,
}

/// Everything about a client that a request can change. Redirect policy is
/// deliberately absent — it is always `none`, because `execute` follows the
/// chain itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClientKey {
    timeout_ms: u64,
    verify_tls: bool,
    proxy: Option<String>,
    /// Fingerprint of the client certificate, so a pooled client is never
    /// handed back presenting somebody else's identity — or none at all.
    identity: Option<u64>,
}

/// A client certificate and its private key, as PEM.
struct IdentityMaterial {
    /// Hash of the PEM, used only to key the client pool.
    fingerprint: u64,
    pem: Vec<u8>,
}

/// Written by hand rather than derived: this struct holds a private key, and a
/// derived `Debug` would put it in the first log line that formats a request.
impl std::fmt::Debug for IdentityMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityMaterial")
            .field("fingerprint", &self.fingerprint)
            .field("pem", &format_args!("<{} bytes redacted>", self.pem.len()))
            .finish()
    }
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    /// Epoch milliseconds.
    expires_at_ms: u64,
}

impl Executor {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn execute(
        &mut self,
        spec: &RequestSpec,
        ctx: &mut RunContext<'_, '_>,
    ) -> Result<RunOutcome> {
        let resolved_url = ctx.resolver.resolve(&spec.url)?;
        let mut url = reqwest::Url::parse(&resolved_url).map_err(|e| EngineError::BadUrl {
            url: resolved_url.clone(),
            message: e.to_string(),
        })?;

        let query = ctx.resolver.resolve_map(&spec.query)?;
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query.iter_pairs() {
                pairs.append_pair(key, value);
            }
        }

        let headers = ctx.resolver.resolve_map(&spec.headers)?;
        let mut header_list: Vec<(String, String)> = headers
            .iter_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        let body = self.build_body(spec, ctx)?;
        if let Some(content_type) = body.as_ref().and_then(|b| b.content_type.clone())
            && !headers.contains_key("content-type")
        {
            header_list.push(("Content-Type".to_string(), content_type));
        }

        // Auth that isn't session-derived is applied once, up front; the session
        // cookie is recomputed per hop, because the right cookie depends on
        // where the hop is going.
        self.apply_auth(spec, ctx, &mut url, &mut header_list).await?;

        let identity = resolve_identity(spec, ctx)?;
        let client = self.client_for(spec, identity.as_ref())?;
        let mut method = reqwest::Method::from_bytes(spec.method.as_bytes()).map_err(|e| {
            EngineError::BadUrl { url: spec.method.clone(), message: e.to_string() }
        })?;
        let mut body_bytes = body.as_ref().map(|b| b.bytes.clone());

        let sent_url = url.clone();
        let sent_headers = header_list.clone();

        let started = Instant::now();
        let mut hops: Vec<Hop> = Vec::new();
        let mut ttfb_ms;

        // Cookies set *during* this chain, kept separately from the persistent
        // session. A login that sets its cookie on a 302 must have that cookie
        // on the next hop whether or not the request opted into a session, and
        // whether or not we are persisting anything.
        let mut chain_jar = SessionStore::new();

        // Redirects are followed here rather than inside the HTTP client. The
        // client's own follower hides the intermediate responses, and the
        // intermediate responses are exactly where a login flow puts its
        // `Set-Cookie` — the whole point of this engine is not to lose those.
        let (status, http_version, response_headers, bytes, final_url) = loop {
            let mut request = client.request(method.clone(), url.clone());
            for (key, value) in &header_list {
                request = request.header(key, value);
            }
            // Cookies for *this* hop's host and path.
            if let Some(cookie) = self.hop_cookie(spec, ctx, &chain_jar, &url) {
                request = request.header("Cookie", cookie);
            }
            if let Some(bytes) = &body_bytes {
                request = request.body(bytes.clone());
            }

            let raw = request
                .send()
                .await
                .map_err(|e| EngineError::Transport(describe_transport_error(&e)))?;
            ttfb_ms = started.elapsed().as_millis() as u64;

            let status = raw.status();
            let http_version = format!("{:?}", raw.version());
            let hop_url = raw.url().to_string();

            let mut response_headers = gabriel_core::model::FieldMap::default();
            let mut set_cookies = Vec::new();
            for (name, value) in raw.headers() {
                let value = value.to_str().unwrap_or("<binary header value>").to_string();
                if name.as_str().eq_ignore_ascii_case("set-cookie") {
                    set_cookies.push(value.clone());
                }
                response_headers.insert(name.as_str(), value);
            }

            // Record before following, so a cookie set on a 302 is in the jar
            // by the time the next hop asks for it.
            if !set_cookies.is_empty() {
                let host = url.host_str().unwrap_or_default().to_string();
                let path = url.path().to_string();
                chain_jar.record_set_cookies(
                    CHAIN_SESSION,
                    set_cookies.iter().map(String::as_str),
                    &host,
                    &path,
                );
                if ctx.record_cookies {
                    let session = ctx.session.clone();
                    ctx.sessions.record_set_cookies(
                        &session,
                        set_cookies.iter().map(String::as_str),
                        &host,
                        &path,
                    );
                }
            }

            let location = response_headers.get_first("location").map(str::to_string);
            let should_follow = is_redirect(status.as_u16())
                && spec.settings.follow_redirects
                && hops.len() < spec.settings.max_redirects
                && location.is_some();

            if !should_follow {
                let bytes = raw
                    .bytes()
                    .await
                    .map_err(|e| EngineError::Transport(describe_transport_error(&e)))?;
                break (status, http_version, response_headers, bytes, hop_url);
            }

            let location = location.expect("checked above");
            let next = url.join(&location).map_err(|e| EngineError::BadUrl {
                url: location.clone(),
                message: e.to_string(),
            })?;

            hops.push(Hop { status: status.as_u16(), url: url.to_string(), location: next.to_string() });

            // Credentials must not follow a redirect to another origin. This is
            // how tokens leak: an open redirect on the target hands your
            // `Authorization` header to whoever it points at.
            if !same_origin(&next, &url) {
                strip_credentials(&mut header_list);
            }

            // RFC 9110 §15.4: 303 always becomes GET, and 301/302 are
            // universally treated the same way in practice. 307/308 preserve
            // the method and the body.
            if matches!(status.as_u16(), 301 | 302 | 303) && method != reqwest::Method::GET {
                method = reqwest::Method::GET;
                body_bytes = None;
                header_list.retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
            }

            url = next;
        };

        let total_ms = started.elapsed().as_millis() as u64;

        let response = ExecutedResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("").to_string(),
            http_version,
            headers: response_headers,
            body: bytes.to_vec(),
            timings: Timings { ttfb_ms, total_ms },
            final_url,
        };

        let captured = apply_captures(spec, &response, ctx);
        let assertions = spec
            .asserts
            .iter()
            .map(|a| assertion::evaluate(a, &response))
            .collect();

        Ok(RunOutcome {
            sent: SentRequest {
                method: spec.method.clone(),
                url: sent_url.to_string(),
                headers: sent_headers,
                body: body.and_then(|b| String::from_utf8(b.bytes).ok()),
            },
            response,
            assertions,
            captured,
            redirects: hops,
        })
    }

    /// The `Cookie` header for one hop: the persistent session's cookies when
    /// the request asked to inherit one, plus anything set earlier in this
    /// redirect chain. Chain cookies win, being the fresher value.
    fn hop_cookie(
        &self,
        spec: &RequestSpec,
        ctx: &RunContext<'_, '_>,
        chain_jar: &SessionStore,
        url: &reqwest::Url,
    ) -> Option<String> {
        let host = url.host_str().unwrap_or_default();
        let secure = url.scheme() == "https";

        let from_session = match &spec.auth {
            Some(Auth::Session { session }) => {
                let name = session.clone().unwrap_or_else(|| ctx.session.clone());
                ctx.sessions.cookie_header(&name, host, url.path(), secure)
            }
            _ => None,
        };
        let from_chain = chain_jar.cookie_header(CHAIN_SESSION, host, url.path(), secure);

        merge_cookie_headers(from_session.as_deref(), from_chain.as_deref())
    }

    fn client_for(
        &mut self,
        spec: &RequestSpec,
        identity: Option<&IdentityMaterial>,
    ) -> Result<reqwest::Client> {
        let settings = &spec.settings;
        let key = ClientKey {
            timeout_ms: settings.timeout_ms,
            verify_tls: settings.verify_tls,
            proxy: settings.proxy.clone(),
            identity: identity.map(|i| i.fingerprint),
        };
        if let Some(client) = self.clients.get(&key) {
            return Ok(client.clone());
        }

        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_millis(settings.timeout_ms))
            // `execute` walks the redirect chain itself so it can see, record
            // and re-send cookies at every hop. The client must not race it.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("gabriel/", env!("CARGO_PKG_VERSION")))
            // Cookies are handled explicitly through the session store, so the
            // client's own jar stays out of the way.
            .danger_accept_invalid_certs(!settings.verify_tls);

        if let Some(proxy) = &settings.proxy {
            builder = builder
                .proxy(reqwest::Proxy::all(proxy).map_err(|e| EngineError::Client(e.to_string()))?);
        }

        if let Some(identity) = identity {
            let parsed = reqwest::Identity::from_pem(&identity.pem).map_err(|e| {
                EngineError::ClientCert {
                    reference: settings.client_cert.clone().unwrap_or_default(),
                    message: e.to_string(),
                }
            })?;
            builder = builder.identity(parsed);
        }

        let client = builder.build().map_err(|e| EngineError::Client(e.to_string()))?;
        self.clients.insert(key, client.clone());
        Ok(client)
    }

    fn build_body(
        &self,
        spec: &RequestSpec,
        ctx: &mut RunContext<'_, '_>,
    ) -> Result<Option<PreparedBody>> {
        let Some(body) = &spec.body else {
            return Ok(None);
        };
        let prepared = match body {
            Body::None => return Ok(None),
            Body::Json { content } => PreparedBody {
                bytes: ctx.resolver.resolve(content)?.into_bytes(),
                content_type: Some("application/json".to_string()),
            },
            Body::Text { content, content_type } => PreparedBody {
                bytes: ctx.resolver.resolve(content)?.into_bytes(),
                content_type: Some(
                    content_type.clone().unwrap_or_else(|| "text/plain".to_string()),
                ),
            },
            Body::Form { fields } => {
                let mut encoded = String::new();
                for (key, value) in fields {
                    if !encoded.is_empty() {
                        encoded.push('&');
                    }
                    encoded.push_str(&urlencode(&ctx.resolver.resolve(key)?));
                    encoded.push('=');
                    encoded.push_str(&urlencode(&ctx.resolver.resolve(value)?));
                }
                PreparedBody {
                    bytes: encoded.into_bytes(),
                    content_type: Some("application/x-www-form-urlencoded".to_string()),
                }
            }
            Body::GraphQl { query, variables, operation_name } => {
                let mut payload = serde_json::Map::new();
                payload.insert("query".into(), ctx.resolver.resolve(query)?.into());
                if let Some(vars) = variables {
                    let resolved = ctx.resolver.resolve(vars)?;
                    let parsed: serde_json::Value = serde_json::from_str(&resolved)
                        .map_err(|e| CoreError::Invalid(format!("graphql variables: {e}")))?;
                    payload.insert("variables".into(), parsed);
                }
                if let Some(name) = operation_name {
                    payload.insert("operationName".into(), ctx.resolver.resolve(name)?.into());
                }
                PreparedBody {
                    bytes: serde_json::to_vec(&payload).expect("map serializes"),
                    content_type: Some("application/json".to_string()),
                }
            }
            Body::File { path, content_type } => {
                let resolved = ctx.resolver.resolve(path)?;
                let full = ctx.base_dir.join(&resolved);
                let bytes = std::fs::read(&full)
                    .map_err(|source| EngineError::BodyFile { path: full.clone(), source })?;
                PreparedBody {
                    bytes,
                    content_type: content_type
                        .clone()
                        .or_else(|| Some("application/octet-stream".to_string())),
                }
            }
        };
        Ok(Some(prepared))
    }

    async fn apply_auth(
        &mut self,
        spec: &RequestSpec,
        ctx: &mut RunContext<'_, '_>,
        url: &mut reqwest::Url,
        headers: &mut Vec<(String, String)>,
    ) -> Result<()> {
        let Some(auth) = &spec.auth else {
            return Ok(());
        };
        match auth {
            Auth::None | Auth::Inherit => {}

            // The differentiator: send the cookies the browser already holds.
            // Applied per hop by `session_cookie`, not here, because a redirect
            // can move the request to a host with different cookies.
            Auth::Session { .. } => {}

            Auth::Bearer { token } => {
                let token = ctx.resolver.resolve(token)?;
                headers.push(("Authorization".to_string(), format!("Bearer {token}")));
            }

            Auth::Basic { username, password } => {
                let username = ctx.resolver.resolve(username)?;
                let password = ctx.resolver.resolve(password)?;
                let encoded = gabriel_core::b64_encode(format!("{username}:{password}").as_bytes());
                headers.push(("Authorization".to_string(), format!("Basic {encoded}")));
            }

            Auth::ApiKey { key, value, location } => {
                let key = ctx.resolver.resolve(key)?;
                let value = ctx.resolver.resolve(value)?;
                match location {
                    ApiKeyLocation::Header => headers.push((key, value)),
                    ApiKeyLocation::Query => {
                        url.query_pairs_mut().append_pair(&key, &value);
                    }
                }
            }

            Auth::OAuth2(config) => {
                let token = self.oauth_token(config, ctx).await?;
                headers.push(("Authorization".to_string(), format!("Bearer {token}")));
            }
        }
        Ok(())
    }

    async fn oauth_token(
        &mut self,
        config: &OAuth2,
        ctx: &mut RunContext<'_, '_>,
    ) -> Result<String> {
        let token_url = ctx.resolver.resolve(&config.token_url)?;
        let client_id = ctx.resolver.resolve(&config.client_id)?;
        let scope = config.scope.as_ref().map(|s| ctx.resolver.resolve(s)).transpose()?;

        let cache_key = format!("{token_url}|{client_id}|{}", scope.clone().unwrap_or_default());
        // Refresh 30s early: a token that expires mid-flight is a flaky test.
        if let Some(cached) = self.oauth_tokens.get(&cache_key)
            && cached.expires_at_ms > gabriel_core::now_ms() + 30_000
        {
            return Ok(cached.token.clone());
        }

        let client_secret = config
            .client_secret
            .as_ref()
            .map(|s| ctx.resolver.resolve(s))
            .transpose()?;

        let grant = match config.grant {
            OAuth2Grant::ClientCredentials => "client_credentials",
            OAuth2Grant::Password => "password",
        };
        let mut form: Vec<(String, String)> = vec![("grant_type".into(), grant.into())];
        if let Some(scope) = &scope {
            form.push(("scope".into(), scope.clone()));
        }
        if let Some(audience) = &config.audience {
            form.push(("audience".into(), ctx.resolver.resolve(audience)?));
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| EngineError::Client(e.to_string()))?;
        let mut request = client.post(&token_url);

        if config.credentials_in_body {
            form.push(("client_id".into(), client_id.clone()));
            if let Some(secret) = &client_secret {
                form.push(("client_secret".into(), secret.clone()));
            }
        } else {
            let encoded = gabriel_core::b64_encode(
                format!("{client_id}:{}", client_secret.clone().unwrap_or_default()).as_bytes(),
            );
            request = request.header("Authorization", format!("Basic {encoded}"));
        }

        let response = request
            .form(&form)
            .send()
            .await
            .map_err(|e| EngineError::Auth(format!("token request to {token_url} failed: {e}")))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(EngineError::Auth(format!(
                "token endpoint {token_url} returned {status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }

        let payload: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| EngineError::Auth(format!("token response was not JSON: {e}")))?;
        let token = payload
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EngineError::Auth("token response has no access_token".into()))?
            .to_string();
        let expires_in = payload.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(3600);

        self.oauth_tokens.insert(
            cache_key,
            CachedToken {
                token: token.clone(),
                expires_at_ms: gabriel_core::now_ms() + expires_in * 1000,
            },
        );
        Ok(token)
    }
}

struct PreparedBody {
    bytes: Vec<u8>,
    content_type: Option<String>,
}

/// Bind response values to variables so the next request can use them.
fn apply_captures(
    spec: &RequestSpec,
    response: &ExecutedResponse,
    ctx: &mut RunContext<'_, '_>,
) -> Vec<(String, String)> {
    let mut captured = Vec::new();
    let json = response.json();

    for capture in &spec.captures {
        let value = match capture.from {
            CaptureSource::Status => Some(response.status.to_string()),
            CaptureSource::Header => capture
                .path
                .as_deref()
                .and_then(|name| response.headers.get_first(name))
                .map(str::to_string),
            CaptureSource::Cookie => capture.path.as_deref().and_then(|name| {
                response
                    .headers
                    .iter_pairs()
                    .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
                    .find_map(|(_, v)| {
                        let (cookie_name, rest) = v.split_once('=')?;
                        (cookie_name.trim() == name)
                            .then(|| rest.split(';').next().unwrap_or("").trim().to_string())
                    })
            }),
            CaptureSource::Body => match (&json, capture.path.as_deref()) {
                (Some(json), Some(path)) => {
                    jsonpath::select(json, path).ok().flatten().map(jsonpath::to_plain_string)
                }
                (_, None) => Some(response.text().into_owned()),
                (None, Some(_)) => None,
            },
        };

        if let Some(value) = value {
            ctx.resolver.set(&capture.var, &value);
            captured.push((capture.var.clone(), value));
        }
    }
    captured
}

/// Load the client certificate named by `settings.client_cert`, if any.
///
/// The value is resolved as a template first, so `{{secret:name}}` pulls a PEM
/// straight out of the vault and the private key never has to sit on disk. A
/// value that does not look like PEM is treated as a path, relative to the
/// collection — the same thing `curl --cert` accepts.
fn resolve_identity(
    spec: &RequestSpec,
    ctx: &mut RunContext<'_, '_>,
) -> Result<Option<IdentityMaterial>> {
    use std::hash::{Hash as _, Hasher as _};

    let Some(reference) = &spec.settings.client_cert else {
        return Ok(None);
    };
    let resolved = ctx.resolver.resolve(reference)?;
    if resolved.trim().is_empty() {
        return Ok(None);
    }

    let pem = if resolved.contains("-----BEGIN") {
        resolved.into_bytes()
    } else {
        let path = ctx.base_dir.join(&resolved);
        std::fs::read(&path).map_err(|source| EngineError::ClientCertFile { path, source })?
    };

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    pem.hash(&mut hasher);
    Ok(Some(IdentityMaterial { fingerprint: hasher.finish(), pem }))
}

/// reqwest's own message stops at "error sending request"; the cause is in the
/// source chain, and the cause is the part a developer needs.
fn describe_transport_error(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let text = cause.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = cause.source();
    }
    message
}

fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gabriel_core::model::{AssertOp, AssertTarget, Assertion, FieldMap, VarCapture};

    fn response_with(status: u16, headers: &[(&str, &str)], body: &str) -> ExecutedResponse {
        ExecutedResponse {
            status,
            status_text: String::new(),
            http_version: "HTTP/1.1".into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<FieldMap>(),
            body: body.as_bytes().to_vec(),
            timings: Timings::default(),
            final_url: "https://api.test".into(),
        }
    }

    #[test]
    fn captures_bind_body_headers_status_and_cookies() {
        let mut spec = RequestSpec::new("GET", "https://api.test");
        spec.captures = vec![
            VarCapture { var: "id".into(), from: CaptureSource::Body, path: Some("data.id".into()) },
            VarCapture {
                var: "region".into(),
                from: CaptureSource::Header,
                path: Some("x-region".into()),
            },
            VarCapture { var: "code".into(), from: CaptureSource::Status, path: None },
            VarCapture { var: "sid".into(), from: CaptureSource::Cookie, path: Some("sid".into()) },
        ];

        let response = response_with(
            201,
            &[("X-Region", "eu-west-1"), ("Set-Cookie", "sid=abc123; Path=/")],
            r#"{"data":{"id":"u_7"}}"#,
        );

        let mut resolver = Resolver::new();
        let mut sessions = SessionStore::new();
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);
        let captured = apply_captures(&spec, &response, &mut ctx);

        assert_eq!(
            captured,
            vec![
                ("id".to_string(), "u_7".to_string()),
                ("region".to_string(), "eu-west-1".to_string()),
                ("code".to_string(), "201".to_string()),
                ("sid".to_string(), "abc123".to_string()),
            ]
        );
        // And they are available to the next request in the run.
        assert_eq!(resolver.get("id"), Some("u_7"));
    }

    #[test]
    fn a_capture_that_finds_nothing_binds_nothing() {
        let mut spec = RequestSpec::new("GET", "https://api.test");
        spec.captures = vec![VarCapture {
            var: "id".into(),
            from: CaptureSource::Body,
            path: Some("missing".into()),
        }];
        let response = response_with(200, &[], "{}");

        let mut resolver = Resolver::new();
        let mut sessions = SessionStore::new();
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);

        assert!(apply_captures(&spec, &response, &mut ctx).is_empty());
        assert_eq!(resolver.get("id"), None);
    }

    #[test]
    fn assertions_are_evaluated_against_the_response() {
        let asserts = vec![
            Assertion {
                target: AssertTarget::Status,
                path: None,
                op: AssertOp::Eq,
                value: Some(toml::Value::Integer(200)),
            },
            Assertion {
                target: AssertTarget::Body,
                path: Some("ok".into()),
                op: AssertOp::Eq,
                value: Some(toml::Value::Boolean(true)),
            },
        ];
        let response =
            response_with(200, &[("Content-Type", "application/json")], r#"{"ok":true}"#);
        let outcomes: Vec<_> = asserts.iter().map(|a| assertion::evaluate(a, &response)).collect();
        assert!(outcomes.iter().all(|o| o.passed), "{outcomes:?}");
    }

    #[test]
    fn form_encoding_escapes_reserved_characters() {
        assert_eq!(urlencode("a b&c=d"), "a+b%26c%3Dd");
        assert_eq!(urlencode("héllo"), "h%C3%A9llo");
    }

    #[test]
    fn clients_are_pooled_by_settings() {
        let mut executor = Executor::new();
        let spec = RequestSpec::new("GET", "https://api.test");
        executor.client_for(&spec, None).unwrap();
        executor.client_for(&spec, None).unwrap();
        assert_eq!(executor.clients.len(), 1);

        let mut slower = RequestSpec::new("GET", "https://api.test");
        slower.settings.timeout_ms = 1000;
        executor.client_for(&slower, None).unwrap();
        assert_eq!(executor.clients.len(), 2);
    }

    /// Pooling must not hand back a client carrying the wrong identity — or no
    /// identity at all — to a request that asked for a client certificate.
    #[test]
    fn clients_are_pooled_separately_per_client_certificate() {
        let mut executor = Executor::new();
        let spec = RequestSpec::new("GET", "https://api.test");

        executor.client_for(&spec, None).unwrap();
        assert_eq!(executor.clients.len(), 1);

        let one = IdentityMaterial { fingerprint: 1, pem: Vec::new() };
        let two = IdentityMaterial { fingerprint: 2, pem: Vec::new() };

        // A bad PEM is rejected rather than silently ignored.
        assert!(executor.client_for(&spec, Some(&one)).is_err());
        assert_eq!(executor.clients.len(), 1, "a failed client must not be cached");

        assert_ne!(
            ClientKey {
                timeout_ms: 0,
                verify_tls: true,
                proxy: None,
                identity: Some(one.fingerprint),
            },
            ClientKey {
                timeout_ms: 0,
                verify_tls: true,
                proxy: None,
                identity: Some(two.fingerprint),
            }
        );
    }

    #[test]
    fn a_missing_client_certificate_file_names_the_path() {
        let mut spec = RequestSpec::new("GET", "https://api.test");
        spec.settings.client_cert = Some("certs/absent.pem".into());

        let mut resolver = Resolver::new();
        let mut sessions = SessionStore::new();
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);

        let error = resolve_identity(&spec, &mut ctx).unwrap_err().to_string();
        assert!(error.contains("absent.pem"), "unhelpful error: {error}");
    }

    #[test]
    fn an_inline_pem_is_used_without_touching_the_filesystem() {
        let mut spec = RequestSpec::new("GET", "https://api.test");
        spec.settings.client_cert = Some("{{secret:client_identity}}".into());

        let secrets: std::collections::BTreeMap<String, String> = [(
            "client_identity".to_string(),
            "-----BEGIN CERTIFICATE-----\nnot-a-real-cert\n-----END CERTIFICATE-----".to_string(),
        )]
        .into();
        let mut resolver = Resolver::new().with_secrets(&secrets);
        let mut sessions = SessionStore::new();
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);

        let material = resolve_identity(&spec, &mut ctx).unwrap().expect("identity");
        assert!(String::from_utf8_lossy(&material.pem).contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn no_client_certificate_means_no_identity() {
        let spec = RequestSpec::new("GET", "https://api.test");
        let mut resolver = Resolver::new();
        let mut sessions = SessionStore::new();
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);
        assert!(resolve_identity(&spec, &mut ctx).unwrap().is_none());
    }
}
