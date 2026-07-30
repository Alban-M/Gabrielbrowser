//! The capture proxy.
//!
//! Point a browser at this and every request it makes becomes a capture that
//! can be promoted into an editable request — carrying the session it was made
//! with. That is the loop the whole product exists to serve, and it is the one
//! thing a browser's own DevTools structurally cannot do: DevTools is a
//! read-mostly inspector bound to a tab, not a proxy that can hold a request
//! library, a cookie jar, and a rewrite rule.
//!
//! HTTPS is intercepted with a per-install CA (see [`ca`]). Interception is
//! scoped by host through [`ProxyConfig::exclude`] and [`ProxyConfig::only`]:
//! anything outside that scope is tunnelled byte-for-byte, unread.

pub mod ca;
pub mod store;

use ca::CertificateAuthority;
use gabriel_core::capture::{Capture, CapturedBody, CapturedRequest, CapturedResponse};
use gabriel_core::model::FieldMap;
use gabriel_engine::session::SessionStore;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use store::CaptureStore;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("could not listen on {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Ca(#[from] ca::CaError),

    #[error(transparent)]
    Store(#[from] store::StoreError),
}

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub addr: SocketAddr,
    /// Session name that captured cookies are filed under.
    pub session: String,
    /// Hosts to tunnel without decrypting. A developer proxying their whole
    /// machine should be able to keep their bank out of the capture log.
    pub exclude: Vec<String>,
    /// When set, *only* these hosts are intercepted; everything else tunnels.
    pub only: Vec<String>,
    /// Skip bodies over this size — a 200 MB video should not land in the log.
    pub max_body_bytes: usize,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig {
            addr: ([127, 0, 0, 1], 8888).into(),
            session: gabriel_engine::session::DEFAULT_SESSION.to_string(),
            exclude: Vec::new(),
            only: Vec::new(),
            max_body_bytes: 8 * 1024 * 1024,
        }
    }
}

impl ProxyConfig {
    fn intercepts(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        let matches = |pattern: &String| {
            let pattern = pattern.to_ascii_lowercase();
            host == pattern || host.ends_with(&format!(".{pattern}"))
        };
        if self.exclude.iter().any(matches) {
            return false;
        }
        self.only.is_empty() || self.only.iter().any(matches)
    }
}

struct ProxyState {
    config: ProxyConfig,
    ca: CertificateAuthority,
    store: CaptureStore,
    sessions: Mutex<SessionStore>,
    client: reqwest::Client,
    counter: AtomicU64,
}

pub struct Proxy {
    state: Arc<ProxyState>,
}

/// A running proxy. The bound address is reported back so a caller that asked
/// for port 0 knows where it landed.
pub struct RunningProxy {
    pub addr: SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl RunningProxy {
    pub async fn shutdown(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }
}

impl Proxy {
    pub fn new(
        config: ProxyConfig,
        ca_dir: impl AsRef<std::path::Path>,
        store: CaptureStore,
        sessions: SessionStore,
    ) -> Result<Self, ProxyError> {
        let ca = CertificateAuthority::load_or_create(ca_dir)?;
        let client = reqwest::Client::builder()
            // The proxy must not follow redirects on the browser's behalf: the
            // browser needs to see the 302 to update its own state.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("default client builds");

        Ok(Proxy {
            state: Arc::new(ProxyState {
                config,
                ca,
                store,
                sessions: Mutex::new(sessions),
                client,
                counter: AtomicU64::new(0),
            }),
        })
    }

    pub fn ca_cert_path(&self) -> &std::path::Path {
        self.state.ca.cert_path()
    }

    /// Bind and serve until the returned handle is shut down.
    pub async fn start(self) -> Result<RunningProxy, ProxyError> {
        let listener = TcpListener::bind(self.state.config.addr)
            .await
            .map_err(|source| ProxyError::Bind { addr: self.state.config.addr, source })?;
        let addr = listener.local_addr().unwrap_or(self.state.config.addr);

        let state = self.state.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _peer)) = listener.accept().await else {
                    continue;
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle_request(req, state.clone()));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .with_upgrades()
                        .await;
                });
            }
        });

        Ok(RunningProxy { addr, handle })
    }
}

type ProxyResponse = Response<Full<Bytes>>;

async fn handle_request(
    req: Request<Incoming>,
    state: Arc<ProxyState>,
) -> Result<ProxyResponse, std::convert::Infallible> {
    if req.method() == Method::CONNECT {
        return Ok(handle_connect(req, state));
    }

    // A plain proxied request carries an absolute URI.
    let url = req.uri().to_string();
    Ok(forward(req, url, state).await.unwrap_or_else(bad_gateway))
}

/// `CONNECT host:443` — either intercept with our own certificate, or splice
/// the two sockets together and stay out of it.
fn handle_connect(req: Request<Incoming>, state: Arc<ProxyState>) -> ProxyResponse {
    let Some(authority) = req.uri().authority().map(|a| a.to_string()) else {
        return status_response(StatusCode::BAD_REQUEST, "CONNECT without an authority");
    };
    let host = authority.split(':').next().unwrap_or(&authority).to_string();
    let intercept = state.config.intercepts(&host);

    tokio::spawn(async move {
        let Ok(upgraded) = hyper::upgrade::on(req).await else {
            return;
        };
        let io = TokioIo::new(upgraded);
        if intercept {
            let _ = intercept_tls(io, host, state).await;
        } else {
            let _ = tunnel(io, authority).await;
        }
    });

    Response::new(Full::new(Bytes::new()))
}

/// Terminate TLS with a certificate for `host`, then serve the plaintext
/// requests inside it.
async fn intercept_tls(
    io: TokioIo<hyper::upgrade::Upgraded>,
    host: String,
    state: Arc<ProxyState>,
) -> std::io::Result<()> {
    let config = state
        .ca
        .server_config(&host)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    let tls = acceptor.accept(io).await?;

    let service = service_fn(move |req: Request<Incoming>| {
        let state = state.clone();
        let host = host.clone();
        async move {
            // Inside the tunnel the URI is origin-form, so the absolute URL has
            // to be rebuilt from the host we were asked to connect to.
            let path = req
                .uri()
                .path_and_query()
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| "/".to_string());
            let url = format!("https://{host}{path}");
            Ok::<_, std::convert::Infallible>(
                forward(req, url, state).await.unwrap_or_else(bad_gateway),
            )
        }
    });

    let _ = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(tls), service)
        .await;
    Ok(())
}

/// Blind passthrough for hosts we were told not to look at.
async fn tunnel(
    mut io: TokioIo<hyper::upgrade::Upgraded>,
    authority: String,
) -> std::io::Result<()> {
    let mut upstream = TcpStream::connect(&authority).await?;
    tokio::io::copy_bidirectional(&mut io, &mut upstream).await?;
    Ok(())
}

/// Send the request upstream, record it, and hand the response back to the
/// browser unchanged.
async fn forward(
    req: Request<Incoming>,
    url: String,
    state: Arc<ProxyState>,
) -> Result<ProxyResponse, String> {
    let started = std::time::Instant::now();
    let method = req.method().clone();
    let version = format!("{:?}", req.version());
    let (parts, body) = req.into_parts();

    let mut request_headers = FieldMap::default();
    let mut page = None;
    for (name, value) in &parts.headers {
        let value = value.to_str().unwrap_or("<binary header value>");
        if name.as_str().eq_ignore_ascii_case("referer") {
            page = Some(value.to_string());
        }
        request_headers.insert(name.as_str(), value);
    }

    let request_body = body
        .collect()
        .await
        .map_err(|e| format!("reading the request body failed: {e}"))?
        .to_bytes();

    let mut upstream = state
        .client
        .request(
            reqwest::Method::from_bytes(method.as_str().as_bytes())
                .map_err(|e| format!("unsupported method: {e}"))?,
            &url,
        )
        .body(request_body.to_vec());
    for (name, value) in &parts.headers {
        // Hop-by-hop headers describe this connection, not the request.
        if is_hop_by_hop(name.as_str()) {
            continue;
        }
        upstream = upstream.header(name.as_str(), value.as_bytes());
    }

    let response = upstream
        .send()
        .await
        .map_err(|e| format!("upstream request to {url} failed: {e}"))?;

    let status = response.status();
    let mut response_headers = FieldMap::default();
    let mut set_cookies = Vec::new();
    for (name, value) in response.headers() {
        let value = value.to_str().unwrap_or("<binary header value>").to_string();
        if name.as_str().eq_ignore_ascii_case("set-cookie") {
            set_cookies.push(value.clone());
        }
        response_headers.insert(name.as_str(), value);
    }

    let response_body = response
        .bytes()
        .await
        .map_err(|e| format!("reading the response body failed: {e}"))?;

    // Cookies the browser was just given are cookies a replayed request needs.
    if !set_cookies.is_empty()
        && let Ok(parsed) = reqwest::Url::parse(&url)
    {
        let host = parsed.host_str().unwrap_or_default().to_string();
        let path = parsed.path().to_string();
        let mut sessions = state.sessions.lock().await;
        sessions.record_set_cookies(
            &state.config.session,
            set_cookies.iter().map(String::as_str),
            &host,
            &path,
        );
        let _ = sessions.save();
    }

    let sequence = state.counter.fetch_add(1, Ordering::Relaxed);
    let capture = Capture {
        id: format!("c{:x}{:04x}", gabriel_core::now_ms(), sequence & 0xffff),
        at: gabriel_core::now_ms(),
        duration_ms: started.elapsed().as_millis() as u64,
        session: Some(state.config.session.clone()),
        page,
        request: CapturedRequest {
            method: method.to_string(),
            url: url.clone(),
            http_version: version,
            headers: request_headers,
            body: body_for_capture(&request_body, state.config.max_body_bytes),
        },
        response: Some(CapturedResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("").to_string(),
            headers: response_headers.clone(),
            body: body_for_capture(&response_body, state.config.max_body_bytes),
        }),
    };
    // A failure to record must not break the page the developer is loading.
    let _ = state.store.append(&capture);

    let mut out = Response::builder().status(status.as_u16());
    for (name, value) in response_headers.iter_pairs() {
        // Content-Length is recomputed by hyper from the buffered body, and a
        // stale Transfer-Encoding would make the response unparseable.
        if is_hop_by_hop(name) || name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        out = out.header(name, value);
    }
    out.body(Full::new(response_body))
        .map_err(|e| format!("building the response failed: {e}"))
}

fn body_for_capture(bytes: &[u8], max: usize) -> Option<CapturedBody> {
    if bytes.is_empty() || bytes.len() > max {
        return None;
    }
    CapturedBody::from_bytes(bytes)
}

fn is_hop_by_hop(name: &str) -> bool {
    const HOP_BY_HOP: &[&str] = &[
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ];
    HOP_BY_HOP.contains(&name.to_ascii_lowercase().as_str())
}

fn bad_gateway(message: String) -> ProxyResponse {
    status_response(StatusCode::BAD_GATEWAY, &message)
}

fn status_response(status: StatusCode, message: &str) -> ProxyResponse {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(format!("gabriel proxy: {message}\n"))))
        .expect("static response builds")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(exclude: &[&str], only: &[&str]) -> ProxyConfig {
        ProxyConfig {
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            only: only.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn everything_is_intercepted_by_default() {
        assert!(config(&[], &[]).intercepts("api.test"));
    }

    #[test]
    fn excluded_hosts_and_their_subdomains_are_left_alone() {
        let config = config(&["bank.test"], &[]);
        assert!(!config.intercepts("bank.test"));
        assert!(!config.intercepts("secure.bank.test"));
        assert!(config.intercepts("api.test"));
    }

    #[test]
    fn exclusion_does_not_match_a_lookalike_host() {
        let config = config(&["bank.test"], &[]);
        assert!(
            config.intercepts("notbank.test"),
            "suffix matching must respect the dot boundary"
        );
    }

    #[test]
    fn an_only_list_narrows_interception_to_those_hosts() {
        let config = config(&[], &["api.test"]);
        assert!(config.intercepts("api.test"));
        assert!(config.intercepts("v2.api.test"));
        assert!(!config.intercepts("telemetry.other"));
    }

    #[test]
    fn exclusion_wins_over_inclusion() {
        let config = config(&["internal.api.test"], &["api.test"]);
        assert!(!config.intercepts("internal.api.test"));
        assert!(config.intercepts("public.api.test"));
    }

    #[test]
    fn oversized_bodies_are_not_captured() {
        assert!(body_for_capture(&[0u8; 100], 10).is_none());
        assert!(body_for_capture(b"hello", 10).is_some());
        assert!(body_for_capture(b"", 10).is_none());
    }

    #[test]
    fn hop_by_hop_headers_are_recognised_case_insensitively() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("transfer-encoding"));
        assert!(!is_hop_by_hop("authorization"));
    }
}
