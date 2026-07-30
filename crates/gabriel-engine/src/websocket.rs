//! The WebSocket client.
//!
//! DevTools shows you frames; it cannot send one. That asymmetry is the whole
//! reason this exists: debugging a socket means poking it, not watching it.
//!
//! A socket is described by the same request file as everything else — the URL,
//! headers, and auth all resolve the same way — because a developer should not
//! have to learn a second format to talk to a second protocol. `http(s)://` URLs
//! are accepted and upgraded, since that is what the docs of most services
//! print.

use crate::{EngineError, Result, RunContext, SentRequest};
use futures_util::{SinkExt as _, StreamExt as _};
use gabriel_core::model::{Auth, RequestSpec};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

/// One frame, in either direction, as the caller sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub direction: Direction,
    pub payload: Payload,
    /// Milliseconds since the socket opened.
    pub at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Sent,
    Received,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close { code: Option<u16>, reason: String },
}

impl Payload {
    /// A one-line rendering for a terminal or a log.
    pub fn summary(&self) -> String {
        match self {
            Payload::Text(text) => text.clone(),
            Payload::Binary(bytes) => {
                format!("<{} of binary>", gabriel_core::format_bytes(bytes.len()))
            }
            Payload::Ping(_) => "<ping>".to_string(),
            Payload::Pong(_) => "<pong>".to_string(),
            Payload::Close { code, reason } => match (code, reason.is_empty()) {
                (Some(code), false) => format!("<close {code}: {reason}>"),
                (Some(code), true) => format!("<close {code}>"),
                (None, _) => "<close>".to_string(),
            },
        }
    }

    pub fn json(&self) -> Option<serde_json::Value> {
        match self {
            Payload::Text(text) => serde_json::from_str(text).ok(),
            _ => None,
        }
    }
}

/// What to send, and how long to listen.
#[derive(Debug, Clone)]
pub struct WebSocketPlan {
    /// Text frames to send once the socket opens, in order.
    pub send: Vec<String>,
    /// Stop after this many received frames. Ping/pong do not count.
    pub max_messages: usize,
    /// Stop listening after this long.
    pub max_duration: Duration,
    /// Close the socket after sending, without waiting for more.
    pub close_after_send: bool,
    /// Requested subprotocols (`Sec-WebSocket-Protocol`).
    pub subprotocols: Vec<String>,
}

impl Default for WebSocketPlan {
    fn default() -> Self {
        WebSocketPlan {
            send: Vec::new(),
            max_messages: 50,
            max_duration: Duration::from_secs(30),
            close_after_send: false,
            subprotocols: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WebSocketOutcome {
    pub sent: SentRequest,
    /// The handshake response status — 101 when the upgrade succeeded.
    pub status: u16,
    /// Subprotocol the server selected, if any.
    pub subprotocol: Option<String>,
    pub frames: Vec<Frame>,
    pub ended: SocketEnd,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketEnd {
    /// The server sent a close frame, or the stream ended.
    ClosedByServer,
    /// We closed after sending, as asked.
    ClosedAfterSend,
    MessageLimitReached,
    TimedOut,
}

impl WebSocketOutcome {
    pub fn received(&self) -> impl Iterator<Item = &Frame> {
        self.frames.iter().filter(|f| f.direction == Direction::Received)
    }
}

/// Open a socket, send what was planned, and collect frames until a limit hits.
pub async fn run(
    spec: &RequestSpec,
    ctx: &mut RunContext<'_, '_>,
    plan: &WebSocketPlan,
    mut on_frame: impl FnMut(&Frame),
) -> Result<WebSocketOutcome> {
    let resolved = ctx.resolver.resolve(&spec.url)?;
    let url = to_websocket_url(&resolved)?;

    let mut request = url.as_str().into_client_request().map_err(|e| EngineError::BadUrl {
        url: url.clone(),
        message: e.to_string(),
    })?;

    // Headers, auth and session cookies work exactly as they do for HTTP.
    let headers = ctx.resolver.resolve_map(&spec.headers)?;
    let mut header_list: Vec<(String, String)> = headers
        .iter_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    match &spec.auth {
        Some(Auth::Bearer { token }) => {
            let token = ctx.resolver.resolve(token)?;
            header_list.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        Some(Auth::Basic { username, password }) => {
            let username = ctx.resolver.resolve(username)?;
            let password = ctx.resolver.resolve(password)?;
            let encoded = gabriel_core::b64_encode(format!("{username}:{password}").as_bytes());
            header_list.push(("Authorization".to_string(), format!("Basic {encoded}")));
        }
        Some(Auth::Session { session }) => {
            let name = session.clone().unwrap_or_else(|| ctx.session.clone());
            let parsed = reqwest::Url::parse(&url).ok();
            if let Some(parsed) = parsed
                && let Some(cookie) = ctx.sessions.cookie_header(
                    &name,
                    parsed.host_str().unwrap_or_default(),
                    parsed.path(),
                    parsed.scheme() == "wss",
                )
            {
                header_list.push(("Cookie".to_string(), cookie));
            }
        }
        // An API key in a query string is already in the URL; OAuth would need a
        // token fetch, which a socket handshake is the wrong place for.
        _ => {}
    }

    if !plan.subprotocols.is_empty() {
        header_list
            .push(("Sec-WebSocket-Protocol".to_string(), plan.subprotocols.join(", ")));
    }

    for (name, value) in &header_list {
        let name = tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| EngineError::Invalid(format!("header `{name}`: {e}")))?;
        let value = value
            .parse()
            .map_err(|e| EngineError::Invalid(format!("header value: {e}")))?;
        request.headers_mut().insert(name, value);
    }

    let sent = SentRequest {
        method: "GET".to_string(),
        url: url.clone(),
        headers: header_list,
        body: None,
    };

    let started = Instant::now();
    let (mut socket, response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| EngineError::Transport(format!("websocket handshake failed: {e}")))?;

    let status = response.status().as_u16();
    let subprotocol = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let mut frames = Vec::new();
    let record = |frame: Frame, frames: &mut Vec<Frame>, on_frame: &mut dyn FnMut(&Frame)| {
        on_frame(&frame);
        frames.push(frame);
    };

    for text in &plan.send {
        let text = ctx.resolver.resolve(text)?;
        socket
            .send(Message::Text(Utf8Bytes::from(text.clone())))
            .await
            .map_err(|e| EngineError::Transport(format!("sending failed: {e}")))?;
        record(
            Frame {
                direction: Direction::Sent,
                payload: Payload::Text(text),
                at_ms: started.elapsed().as_millis() as u64,
            },
            &mut frames,
            &mut on_frame,
        );
    }

    if plan.close_after_send {
        let _ = socket.close(None).await;
        return Ok(WebSocketOutcome {
            sent,
            status,
            subprotocol,
            frames,
            ended: SocketEnd::ClosedAfterSend,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }

    let mut received = 0usize;
    let ended = loop {
        let remaining = plan.max_duration.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break SocketEnd::TimedOut;
        }
        match tokio::time::timeout(remaining, socket.next()).await {
            Err(_) => break SocketEnd::TimedOut,
            Ok(None) => break SocketEnd::ClosedByServer,
            Ok(Some(Err(e))) => {
                return Err(EngineError::Transport(format!("websocket error: {e}")));
            }
            Ok(Some(Ok(message))) => {
                let at_ms = started.elapsed().as_millis() as u64;
                let closing = matches!(message, Message::Close(_));
                let Some(payload) = payload_of(message) else {
                    // A raw frame carries nothing a caller can act on.
                    continue;
                };
                // Keep-alives are recorded but do not count toward the limit; a
                // server pinging every second should not end the session.
                let counts = !matches!(payload, Payload::Ping(_) | Payload::Pong(_));
                record(
                    Frame { direction: Direction::Received, payload, at_ms },
                    &mut frames,
                    &mut on_frame,
                );
                if closing {
                    break SocketEnd::ClosedByServer;
                }
                if counts {
                    received += 1;
                    if received >= plan.max_messages {
                        break SocketEnd::MessageLimitReached;
                    }
                }
            }
        }
    };

    // Be polite on the way out; a server that logs unclean closes should not
    // log ours.
    if ended != SocketEnd::ClosedByServer {
        let _ = socket
            .close(Some(CloseFrame {
                code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                reason: Utf8Bytes::from_static("done"),
            }))
            .await;
    }

    Ok(WebSocketOutcome {
        sent,
        status,
        subprotocol,
        frames,
        ended,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

fn payload_of(message: Message) -> Option<Payload> {
    Some(match message {
        Message::Text(text) => Payload::Text(text.to_string()),
        Message::Binary(bytes) => Payload::Binary(bytes.to_vec()),
        Message::Ping(bytes) => Payload::Ping(bytes.to_vec()),
        Message::Pong(bytes) => Payload::Pong(bytes.to_vec()),
        Message::Close(frame) => Payload::Close {
            code: frame.as_ref().map(|f| u16::from(f.code)),
            reason: frame.map(|f| f.reason.to_string()).unwrap_or_default(),
        },
        Message::Frame(_) => return None,
    })
}

/// Accept the URL forms people actually have to hand.
///
/// Service documentation prints `https://` as often as `wss://`, and a URL
/// copied from a browser's network tab is `https://`. Rejecting those would be
/// pedantry.
pub fn to_websocket_url(url: &str) -> Result<String> {
    let trimmed = url.trim();
    let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| EngineError::BadUrl {
        url: trimmed.to_string(),
        message: "expected a ws://, wss://, http:// or https:// URL".to_string(),
    })?;
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "ws" | "http" => "ws",
        "wss" | "https" => "wss",
        other => {
            return Err(EngineError::BadUrl {
                url: trimmed.to_string(),
                message: format!("`{other}` is not a websocket scheme"),
            });
        }
    };
    if rest.is_empty() {
        return Err(EngineError::BadUrl {
            url: trimmed.to_string(),
            message: "no host".to_string(),
        });
    }
    Ok(format!("{scheme}://{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_schemes_are_accepted_as_they_are() {
        assert_eq!(to_websocket_url("ws://api.test/socket").unwrap(), "ws://api.test/socket");
        assert_eq!(to_websocket_url("wss://api.test/socket").unwrap(), "wss://api.test/socket");
    }

    #[test]
    fn http_urls_are_upgraded_because_that_is_what_docs_print() {
        assert_eq!(to_websocket_url("http://api.test/ws").unwrap(), "ws://api.test/ws");
        assert_eq!(to_websocket_url("https://api.test/ws").unwrap(), "wss://api.test/ws");
        // Including case variations and surrounding whitespace.
        assert_eq!(to_websocket_url("  HTTPS://api.test/ws  ").unwrap(), "wss://api.test/ws");
    }

    #[test]
    fn query_strings_and_ports_survive() {
        assert_eq!(
            to_websocket_url("https://api.test:8443/ws?token=abc&v=2").unwrap(),
            "wss://api.test:8443/ws?token=abc&v=2"
        );
    }

    #[test]
    fn other_schemes_are_rejected_with_a_reason() {
        for bad in ["ftp://api.test/", "api.test/ws", "", "wss://"] {
            let error = to_websocket_url(bad).unwrap_err().to_string();
            assert!(!error.is_empty(), "no explanation for {bad:?}");
        }
        assert!(to_websocket_url("ftp://x/").unwrap_err().to_string().contains("not a websocket"));
    }

    #[test]
    fn payload_summaries_stay_on_one_line() {
        assert_eq!(Payload::Text("hello".into()).summary(), "hello");
        assert_eq!(Payload::Binary(vec![0; 2048]).summary(), "<2.0 KB of binary>");
        assert_eq!(Payload::Ping(vec![]).summary(), "<ping>");
        assert_eq!(
            Payload::Close { code: Some(1000), reason: "bye".into() }.summary(),
            "<close 1000: bye>"
        );
        assert_eq!(Payload::Close { code: Some(1001), reason: String::new() }.summary(), "<close 1001>");
        assert_eq!(Payload::Close { code: None, reason: String::new() }.summary(), "<close>");
    }

    #[test]
    fn text_payloads_expose_their_json() {
        assert_eq!(
            Payload::Text(r#"{"type":"tick"}"#.into()).json().unwrap()["type"],
            serde_json::json!("tick")
        );
        assert!(Payload::Text("not json".into()).json().is_none());
        assert!(Payload::Binary(vec![1, 2]).json().is_none());
    }
}
