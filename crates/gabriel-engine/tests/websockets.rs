//! WebSocket behaviour against a real server.
//!
//! The URL handling is unit-tested; everything here needs an actual handshake:
//! that frames go out and come back, that a limit stops the session, that a
//! server-initiated close is noticed, that ping/pong keep-alives do not count as
//! messages, and that auth headers survive the upgrade.

use gabriel_core::model::{Auth, RequestSpec};
use gabriel_core::vars::Resolver;
use gabriel_engine::session::SessionStore;
use gabriel_engine::websocket::{self, Direction, Payload, SocketEnd, WebSocketPlan};
use gabriel_engine::RunContext;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

/// What the test server should do once a client connects.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Reply to each text frame with `echo: <text>`.
    Echo,
    /// Send a burst of frames, unprompted.
    Push(usize),
    /// Send one frame, then a close.
    CloseAfterOne,
    /// Send pings only — never a message.
    PingOnly,
    /// Accept and say nothing at all.
    Silent,
}

/// Headers the server saw on the upgrade request, for assertions.
type SeenHeaders = Arc<Mutex<Vec<(String, String)>>>;

async fn serve(behaviour: Behaviour, subprotocol: Option<&'static str>) -> (SocketAddr, SeenHeaders) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen: SeenHeaders = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let recorder = recorder.clone();
            tokio::spawn(async move {
                use futures_util::{SinkExt as _, StreamExt as _};

                // Record the request headers, and negotiate a subprotocol.
                let callback = |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                                mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    let mut headers = recorder.lock().unwrap();
                    for (name, value) in request.headers() {
                        headers.push((
                            name.as_str().to_string(),
                            value.to_str().unwrap_or_default().to_string(),
                        ));
                    }
                    if let Some(protocol) = subprotocol {
                        response
                            .headers_mut()
                            .insert("sec-websocket-protocol", protocol.parse().unwrap());
                    }
                    Ok(response)
                };

                let Ok(mut socket) =
                    tokio_tungstenite::accept_hdr_async(stream, callback).await
                else {
                    return;
                };

                match behaviour {
                    Behaviour::Echo => {
                        while let Some(Ok(message)) = socket.next().await {
                            match message {
                                Message::Text(text) => {
                                    let reply = format!("echo: {text}");
                                    if socket.send(Message::Text(Utf8Bytes::from(reply))).await.is_err() {
                                        return;
                                    }
                                }
                                Message::Close(_) => return,
                                _ => {}
                            }
                        }
                    }
                    Behaviour::Push(count) => {
                        for i in 0..count {
                            let frame = format!("{{\"tick\":{i}}}");
                            if socket.send(Message::Text(Utf8Bytes::from(frame))).await.is_err() {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        // Then hold the socket open.
                        while socket.next().await.is_some() {}
                    }
                    Behaviour::CloseAfterOne => {
                        let _ = socket.send(Message::Text(Utf8Bytes::from_static("last word"))).await;
                        let _ = socket.close(None).await;
                    }
                    Behaviour::PingOnly => {
                        for _ in 0..5 {
                            if socket.send(Message::Ping(vec![].into())).await.is_err() {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        while socket.next().await.is_some() {}
                    }
                    Behaviour::Silent => {
                        while socket.next().await.is_some() {}
                    }
                }
            });
        }
    });

    (addr, seen)
}

fn spec(addr: SocketAddr) -> RequestSpec {
    RequestSpec::new("GET", format!("ws://{addr}/socket"))
}

async fn run(
    spec: &RequestSpec,
    plan: &WebSocketPlan,
) -> gabriel_engine::Result<gabriel_engine::websocket::WebSocketOutcome> {
    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);
    websocket::run(spec, &mut ctx, plan, |_| {}).await
}

#[tokio::test]
async fn a_frame_sent_comes_back_from_the_server() {
    let (addr, _) = serve(Behaviour::Echo, None).await;
    let plan = WebSocketPlan {
        send: vec!["hello socket".to_string()],
        max_messages: 1,
        ..Default::default()
    };

    let outcome = run(&spec(addr), &plan).await.expect("socket");

    assert_eq!(outcome.status, 101, "the handshake should have upgraded");
    assert_eq!(outcome.frames.len(), 2, "one sent, one received");
    assert_eq!(outcome.frames[0].direction, Direction::Sent);
    assert_eq!(outcome.frames[0].payload, Payload::Text("hello socket".into()));
    assert_eq!(outcome.frames[1].direction, Direction::Received);
    assert_eq!(outcome.frames[1].payload, Payload::Text("echo: hello socket".into()));
}

#[tokio::test]
async fn several_frames_are_sent_in_order() {
    let (addr, _) = serve(Behaviour::Echo, None).await;
    let plan = WebSocketPlan {
        send: vec!["one".into(), "two".into(), "three".into()],
        max_messages: 3,
        ..Default::default()
    };

    let outcome = run(&spec(addr), &plan).await.expect("socket");
    let replies: Vec<String> = outcome
        .received()
        .filter_map(|f| match &f.payload {
            Payload::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(replies, vec!["echo: one", "echo: two", "echo: three"]);
}

#[tokio::test]
async fn templates_in_a_sent_frame_are_resolved() {
    let (addr, _) = serve(Behaviour::Echo, None).await;
    let plan = WebSocketPlan {
        send: vec![r#"{"subscribe":"{{channel}}"}"#.to_string()],
        max_messages: 1,
        ..Default::default()
    };

    let mut resolver =
        Resolver::new().with_vars([("channel".to_string(), "orders".to_string())].into());
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);
    let outcome = websocket::run(&spec(addr), &mut ctx, &plan, |_| {}).await.expect("socket");

    assert_eq!(
        outcome.frames[0].payload,
        Payload::Text(r#"{"subscribe":"orders"}"#.into())
    );
}

#[tokio::test]
async fn the_message_limit_ends_the_session() {
    let (addr, _) = serve(Behaviour::Push(20), None).await;
    let plan = WebSocketPlan { max_messages: 3, ..Default::default() };

    let outcome = run(&spec(addr), &plan).await.expect("socket");
    assert_eq!(outcome.ended, SocketEnd::MessageLimitReached);
    assert_eq!(outcome.received().count(), 3);
}

#[tokio::test]
async fn a_close_from_the_server_is_reported() {
    let (addr, _) = serve(Behaviour::CloseAfterOne, None).await;
    let plan = WebSocketPlan { max_messages: 50, ..Default::default() };

    let outcome = run(&spec(addr), &plan).await.expect("socket");
    assert_eq!(outcome.ended, SocketEnd::ClosedByServer);
    // The last message arrived before the close.
    assert!(
        outcome
            .received()
            .any(|f| f.payload == Payload::Text("last word".into())),
        "the final frame was lost: {:?}",
        outcome.frames
    );
}

/// A server that pings every second must not exhaust the message budget — the
/// budget is for messages a developer cares about.
#[tokio::test]
async fn keepalive_pings_do_not_count_as_messages() {
    let (addr, _) = serve(Behaviour::PingOnly, None).await;
    let plan = WebSocketPlan {
        max_messages: 2,
        max_duration: Duration::from_millis(300),
        ..Default::default()
    };

    let outcome = run(&spec(addr), &plan).await.expect("socket");
    assert_eq!(outcome.ended, SocketEnd::TimedOut, "pings ended the session early");
    assert!(
        outcome.received().any(|f| matches!(f.payload, Payload::Ping(_))),
        "pings should still be recorded"
    );
}

#[tokio::test]
async fn a_silent_socket_times_out_instead_of_hanging() {
    let (addr, _) = serve(Behaviour::Silent, None).await;
    let plan = WebSocketPlan { max_duration: Duration::from_millis(250), ..Default::default() };

    let started = std::time::Instant::now();
    let outcome = run(&spec(addr), &plan).await.expect("socket");

    assert_eq!(outcome.ended, SocketEnd::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(3), "it hung");
}

#[tokio::test]
async fn close_after_send_does_not_wait_for_a_reply() {
    let (addr, _) = serve(Behaviour::Silent, None).await;
    let plan = WebSocketPlan {
        send: vec!["fire and forget".into()],
        close_after_send: true,
        // A budget long enough that waiting would be obvious.
        max_duration: Duration::from_secs(20),
        ..Default::default()
    };

    let started = std::time::Instant::now();
    let outcome = run(&spec(addr), &plan).await.expect("socket");

    assert_eq!(outcome.ended, SocketEnd::ClosedAfterSend);
    assert_eq!(outcome.received().count(), 0);
    assert!(started.elapsed() < Duration::from_secs(2), "it waited anyway");
}

#[tokio::test]
async fn auth_and_custom_headers_travel_with_the_upgrade() {
    let (addr, seen) = serve(Behaviour::Silent, None).await;

    let mut spec = spec(addr);
    spec.auth = Some(Auth::Bearer { token: "socket-token-123".into() });
    spec.headers.set("X-Client", "gabriel-test");

    let plan = WebSocketPlan { max_duration: Duration::from_millis(150), ..Default::default() };
    run(&spec, &plan).await.expect("socket");

    let headers = seen.lock().unwrap();
    let find = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    assert_eq!(find("authorization").as_deref(), Some("Bearer socket-token-123"));
    assert_eq!(find("x-client").as_deref(), Some("gabriel-test"));
}

#[tokio::test]
async fn a_session_cookie_is_sent_on_the_upgrade() {
    let (addr, seen) = serve(Behaviour::Silent, None).await;

    let mut spec = spec(addr);
    spec.auth = Some(Auth::Session { session: Some("work".into()) });

    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    sessions.record_set_cookies("work", ["sid=socket-session"], "127.0.0.1", "/");
    let plan = WebSocketPlan { max_duration: Duration::from_millis(150), ..Default::default() };
    {
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);
        websocket::run(&spec, &mut ctx, &plan, |_| {}).await.expect("socket");
    }

    let headers = seen.lock().unwrap();
    let cookie = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("cookie"))
        .map(|(_, v)| v.clone());
    assert_eq!(cookie.as_deref(), Some("sid=socket-session"));
}

#[tokio::test]
async fn a_negotiated_subprotocol_is_reported() {
    let (addr, seen) = serve(Behaviour::Silent, Some("graphql-ws")).await;

    let plan = WebSocketPlan {
        subprotocols: vec!["graphql-ws".into(), "graphql-transport-ws".into()],
        max_duration: Duration::from_millis(150),
        ..Default::default()
    };
    let outcome = run(&spec(addr), &plan).await.expect("socket");

    assert_eq!(outcome.subprotocol.as_deref(), Some("graphql-ws"));
    let headers = seen.lock().unwrap();
    let requested = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("sec-websocket-protocol"))
        .map(|(_, v)| v.clone());
    assert_eq!(requested.as_deref(), Some("graphql-ws, graphql-transport-ws"));
}

#[tokio::test]
async fn frames_are_delivered_to_the_callback_as_they_arrive() {
    let (addr, _) = serve(Behaviour::Push(4), None).await;
    let plan = WebSocketPlan { max_messages: 4, ..Default::default() };

    let start = std::time::Instant::now();
    let timings = Arc::new(Mutex::new(Vec::new()));
    let recorder = timings.clone();

    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);
    websocket::run(&spec(addr), &mut ctx, &plan, |frame| {
        if frame.direction == Direction::Received {
            recorder.lock().unwrap().push(start.elapsed());
        }
    })
    .await
    .expect("socket");

    let seen = timings.lock().unwrap();
    assert_eq!(seen.len(), 4);
    assert!(
        seen[3] > seen[0],
        "frames were surfaced together rather than as they arrived"
    );
}

#[tokio::test]
async fn a_refused_connection_reports_a_handshake_failure() {
    // Nothing listening on this port.
    let spec = RequestSpec::new("GET", "ws://127.0.0.1:1/socket");
    let error = run(&spec, &WebSocketPlan::default()).await.unwrap_err().to_string();
    assert!(error.contains("handshake"), "unhelpful error: {error}");
}
