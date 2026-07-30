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
use futures_util::TryStreamExt as _;
use gabriel_core::capture::{Capture, CapturedBody, CapturedRequest, CapturedResponse};
use gabriel_core::model::FieldMap;
use gabriel_engine::session::SessionStore;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use store::CaptureStore;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// The proxy's response body. Boxed because a response is either buffered (so
/// it can be captured) or streamed straight through (so it can't be, but also
/// can't stall).
type ProxyBody = BoxBody<Bytes, std::io::Error>;

fn buffered(bytes: Bytes) -> ProxyBody {
    Full::new(bytes)
        .map_err(|never: std::convert::Infallible| match never {})
        .boxed()
}

fn empty_body() -> ProxyBody {
    Empty::<Bytes>::new()
        .map_err(|never: std::convert::Infallible| match never {})
        .boxed()
}

/// Any stream whose bytes can be relayed in both directions.
trait IoStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> IoStream for T {}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("could not listen on {addr}")]
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
    /// Used only for upgraded connections, which an HTTP client cannot carry.
    tls_client: tokio_rustls::TlsConnector,
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

        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls_client = tokio_rustls::TlsConnector::from(Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        ));

        Ok(Proxy {
            state: Arc::new(ProxyState {
                config,
                ca,
                store,
                sessions: Mutex::new(sessions),
                client,
                tls_client,
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

type ProxyResponse = Response<ProxyBody>;

async fn handle_request(
    req: Request<Incoming>,
    state: Arc<ProxyState>,
) -> Result<ProxyResponse, std::convert::Infallible> {
    if req.method() == Method::CONNECT {
        return Ok(handle_connect(req, state));
    }

    // A plain proxied request carries an absolute URI.
    let url = req.uri().to_string();
    Ok(dispatch(req, url, state).await)
}

/// Route one request: an upgrade gets spliced, everything else is forwarded.
async fn dispatch(req: Request<Incoming>, url: String, state: Arc<ProxyState>) -> ProxyResponse {
    if upgrade_target(&req).is_some() {
        return relay_upgrade(req, url, state).await.unwrap_or_else(bad_gateway);
    }
    forward(req, url, state).await.unwrap_or_else(bad_gateway)
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

    Response::new(empty_body())
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
            Ok::<_, std::convert::Infallible>(dispatch(req, url, state).await)
        }
    });

    let _ = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(tls), service)
        // Without this, a WebSocket handshake inside the intercepted tunnel
        // completes and then goes nowhere.
        .with_upgrades()
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

/// Whether this request is asking to change protocol — a WebSocket handshake,
/// in practice.
fn upgrade_target<B>(req: &Request<B>) -> Option<String> {
    let connection = req
        .headers()
        .get(hyper::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // `Connection` is a comma-separated token list, so a substring test would
    // match the wrong thing.
    let asks_to_upgrade = connection.split(',').any(|token| token.trim() == "upgrade");
    if !asks_to_upgrade {
        return None;
    }
    req.headers()
        .get(hyper::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|target| target.to_ascii_lowercase())
}

/// Relay a protocol upgrade.
///
/// An upgraded connection cannot go through an HTTP client: after the 101 the
/// bytes are no longer HTTP, so there is nothing to parse and nothing to
/// capture. The handshake is forwarded verbatim and the two sockets are then
/// spliced. Gabriel records that the upgrade happened and stays out of the
/// frames.
async fn relay_upgrade(
    mut req: Request<Incoming>,
    url: String,
    state: Arc<ProxyState>,
) -> Result<ProxyResponse, String> {
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("{url}: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("{url} has no host"))?
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(80);
    let is_tls = matches!(parsed.scheme(), "https" | "wss");

    // Claim the client side of the upgrade before the request is consumed.
    let client_side = hyper::upgrade::on(&mut req);

    let (parts, _body) = req.into_parts();
    let path = parsed[url::Position::BeforePath..].to_string();

    let mut upstream_request = Request::builder().method(parts.method.clone()).uri(&path);
    for (name, value) in parts.headers.iter() {
        // `Connection` and `Upgrade` are hop-by-hop but they *are* the
        // handshake, so they travel; the rest of the hop headers do not.
        let name_str = name.as_str();
        if is_hop_by_hop(name_str)
            && !name_str.eq_ignore_ascii_case("connection")
            && !name_str.eq_ignore_ascii_case("upgrade")
        {
            continue;
        }
        upstream_request = upstream_request.header(name, value);
    }
    let upstream_request = upstream_request
        .body(Empty::<Bytes>::new())
        .map_err(|e| format!("building the upgrade request failed: {e}"))?;

    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("connecting to {host}:{port} failed: {}", describe(&e)))?;
    let io: Box<dyn IoStream> = if is_tls {
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|e| format!("{host} is not a valid server name: {e}"))?;
        let tls = state
            .tls_client
            .connect(name, tcp)
            .await
            .map_err(|e| format!("TLS handshake with {host} failed: {}", describe(&e)))?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };

    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|e| format!("HTTP handshake with {host} failed: {}", describe(&e)))?;
    // The connection task must keep running for the upgrade to complete.
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });

    let mut upstream_response = sender
        .send_request(upstream_request)
        .await
        .map_err(|e| format!("upgrade request to {url} failed: {}", describe(&e)))?;

    let status = upstream_response.status();
    let mut response_headers = FieldMap::default();
    for (name, value) in upstream_response.headers() {
        response_headers.insert(
            name.as_str(),
            value.to_str().unwrap_or("<binary header value>"),
        );
    }

    let sequence = state.counter.fetch_add(1, Ordering::Relaxed);
    let mut request_headers = FieldMap::default();
    for (name, value) in parts.headers.iter() {
        request_headers.insert(name.as_str(), value.to_str().unwrap_or("<binary header value>"));
    }
    let _ = state.store.append(&Capture {
        id: format!("c{:x}{:04x}", gabriel_core::now_ms(), sequence & 0xffff),
        at: gabriel_core::now_ms(),
        duration_ms: 0,
        session: Some(state.config.session.clone()),
        page: None,
        request: CapturedRequest {
            method: parts.method.to_string(),
            url: url.clone(),
            http_version: format!("{:?}", parts.version),
            headers: request_headers,
            body: None,
        },
        response: Some(CapturedResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("").to_string(),
            headers: response_headers.clone(),
            body: None,
        }),
    });

    if status != StatusCode::SWITCHING_PROTOCOLS {
        // The server declined to upgrade; pass its answer along as an ordinary
        // response.
        let bytes = upstream_response
            .body_mut()
            .collect()
            .await
            .map(|collected| collected.to_bytes())
            .unwrap_or_default();
        let mut out = Response::builder().status(status);
        for (name, value) in response_headers.iter_pairs() {
            if !is_hop_by_hop(name) && !name.eq_ignore_ascii_case("content-length") {
                out = out.header(name, value);
            }
        }
        return out
            .body(buffered(bytes))
            .map_err(|e| format!("building the response failed: {e}"));
    }

    let upstream_side = hyper::upgrade::on(&mut upstream_response);
    tokio::spawn(async move {
        if let Ok((client, upstream)) = tokio::try_join!(client_side, upstream_side) {
            let mut client = TokioIo::new(client);
            let mut upstream = TokioIo::new(upstream);
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        }
    });

    let mut out = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
    for (name, value) in response_headers.iter_pairs() {
        out = out.header(name, value);
    }
    out.body(empty_body())
        .map_err(|e| format!("building the 101 response failed: {e}"))
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
        .map_err(|e| format!("reading the request body failed: {}", describe(&e)))?
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
        .map_err(|e| format!("upstream request to {url} failed: {}", describe(&e)))?;

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

    let record = |response_body: Option<&Bytes>| {
        let sequence = state.counter.fetch_add(1, Ordering::Relaxed);
        let capture = Capture {
            id: format!("c{:x}{:04x}", gabriel_core::now_ms(), sequence & 0xffff),
            at: gabriel_core::now_ms(),
            duration_ms: started.elapsed().as_millis() as u64,
            session: Some(state.config.session.clone()),
            page: page.clone(),
            request: CapturedRequest {
                method: method.to_string(),
                url: url.clone(),
                http_version: version.clone(),
                headers: request_headers.clone(),
                body: body_for_capture(&request_body, state.config.max_body_bytes),
            },
            response: Some(CapturedResponse {
                status: status.as_u16(),
                status_text: status.canonical_reason().unwrap_or("").to_string(),
                headers: response_headers.clone(),
                body: response_body
                    .and_then(|bytes| body_for_capture(bytes, state.config.max_body_bytes)),
            }),
        };
        // A failure to record must not break the page the developer is loading.
        let _ = state.store.append(&capture);
    };

    let mut out = Response::builder().status(status.as_u16());
    for (name, value) in response_headers.iter_pairs() {
        // Content-Length is recomputed by hyper from the body it is given, and
        // a stale Transfer-Encoding would make the response unparseable.
        if is_hop_by_hop(name) || name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        out = out.header(name, value);
    }

    // A response that never ends must not be buffered: waiting for the last
    // byte of an event stream means the browser sees nothing at all. Capture
    // gives way to delivery here, and the capture records the headers alone.
    if streams_indefinitely(&response_headers, state.config.max_body_bytes) {
        record(None);
        let stream = response
            .bytes_stream()
            .map_ok(Frame::data)
            .map_err(std::io::Error::other);
        return out
            .body(StreamBody::new(stream).boxed())
            .map_err(|e| format!("building the streaming response failed: {e}"));
    }

    let response_body = response
        .bytes()
        .await
        .map_err(|e| format!("reading the response body failed: {}", describe(&e)))?;
    record(Some(&response_body));

    out.body(buffered(response_body))
        .map_err(|e| format!("building the response failed: {e}"))
}

/// Whether a response body has to be passed through rather than collected.
///
/// Two cases: bodies that are open-ended by design (an event stream has no last
/// byte to wait for), and bodies too large to hold in memory — which the
/// capture would have discarded anyway.
fn streams_indefinitely(headers: &FieldMap, max_body_bytes: usize) -> bool {
    let content_type = headers
        .get_first("content-type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if content_type.starts_with("text/event-stream")
        || content_type.starts_with("multipart/x-mixed-replace")
        || content_type.starts_with("application/grpc")
    {
        return true;
    }
    headers
        .get_first("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > max_body_bytes)
}

/// Flatten an error and its causes into one line.
///
/// reqwest's own message stops at "error sending request"; the reason — a
/// refused connection, a rejected header, a TLS failure — lives in the source
/// chain. Reporting only the top line makes the proxy's 502s undiagnosable.
fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
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
        .body(buffered(Bytes::from(format!("gabriel proxy: {message}\n"))))
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

    fn headers(pairs: &[(&str, &str)]) -> FieldMap {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn event_streams_are_never_buffered() {
        assert!(streams_indefinitely(
            &headers(&[("content-type", "text/event-stream")]),
            8_000_000
        ));
        assert!(streams_indefinitely(
            &headers(&[("Content-Type", "text/event-stream; charset=utf-8")]),
            8_000_000
        ));
    }

    #[test]
    fn ordinary_responses_are_buffered_so_they_can_be_captured() {
        assert!(!streams_indefinitely(
            &headers(&[("content-type", "application/json"), ("content-length", "1024")]),
            8_000_000
        ));
        // A chunked JSON response has no content-length and must still buffer.
        assert!(!streams_indefinitely(
            &headers(&[("content-type", "application/json")]),
            8_000_000
        ));
    }

    #[test]
    fn a_body_too_large_to_capture_is_streamed_instead_of_held() {
        assert!(streams_indefinitely(
            &headers(&[("content-type", "video/mp4"), ("content-length", "900000000")]),
            8_000_000
        ));
    }

    /// `upgrade_target` only reads headers, so the body type is irrelevant —
    /// which is why it is generic and testable without a live connection.
    fn request_with(pairs: &[(&str, &str)]) -> Request<()> {
        let mut builder = Request::builder().uri("http://api.test/socket");
        for (name, value) in pairs {
            builder = builder.header(*name, *value);
        }
        builder.body(()).expect("builds")
    }

    #[test]
    fn a_websocket_handshake_is_recognised() {
        let req = request_with(&[("connection", "Upgrade"), ("upgrade", "websocket")]);
        assert_eq!(upgrade_target(&req).as_deref(), Some("websocket"));
    }

    #[test]
    fn a_connection_token_list_is_parsed_not_substring_matched() {
        let req = request_with(&[("connection", "keep-alive, Upgrade"), ("upgrade", "websocket")]);
        assert_eq!(upgrade_target(&req).as_deref(), Some("websocket"));

        // "upgrade-insecure-requests" must not read as an upgrade request.
        let req = request_with(&[("connection", "keep-alive"), ("upgrade-insecure-requests", "1")]);
        assert_eq!(upgrade_target(&req), None);
    }

    #[test]
    fn an_ordinary_request_is_not_an_upgrade() {
        let req = request_with(&[("accept", "application/json")]);
        assert_eq!(upgrade_target(&req), None);
    }
}
