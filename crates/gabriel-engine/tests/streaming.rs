//! Streaming requests, against a server that trickles events out.
//!
//! The parser is unit-tested; this covers what only a real socket shows: that
//! events surface as they arrive rather than at the end, that limits actually
//! stop the stream, and that a non-stream response is reported instead of
//! hanging until the timeout.

use gabriel_core::model::RequestSpec;
use gabriel_core::vars::Resolver;
use gabriel_engine::session::SessionStore;
use gabriel_engine::{Executor, RunContext, StreamEnd, StreamLimits};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve one response. `chunks` are written with `gap` between them, so the test
/// can tell arrival-as-you-go from arrival-at-the-end.
async fn serve(head: &'static str, chunks: Vec<&'static str>, gap: Duration) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let chunks = chunks.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                if stream.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                let _ = stream.flush().await;
                for chunk in chunks {
                    tokio::time::sleep(gap).await;
                    if stream.write_all(chunk.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = stream.flush().await;
                }
                // Closing ends the stream.
            });
        }
    });

    addr
}

const STREAM_HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";

fn spec(addr: SocketAddr) -> RequestSpec {
    RequestSpec::new("GET", format!("http://{addr}/events"))
}

#[tokio::test]
async fn events_are_delivered_as_they_arrive() {
    let addr = serve(
        STREAM_HEAD,
        vec!["data: one\n\n", "data: two\n\n", "data: three\n\n"],
        Duration::from_millis(60),
    )
    .await;

    // Record when each event was seen relative to the start.
    let start = std::time::Instant::now();
    let timings = Arc::new(Mutex::new(Vec::new()));
    let recorder = timings.clone();

    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);

    let outcome = executor
        .execute_stream(&spec(addr), &mut ctx, &StreamLimits::default(), |event| {
            recorder
                .lock()
                .unwrap()
                .push((event.data.clone(), start.elapsed()));
        })
        .await
        .expect("stream");

    assert_eq!(outcome.status, 200);
    assert_eq!(outcome.ended, StreamEnd::Closed);
    let data: Vec<String> = outcome.events.iter().map(|e| e.data.clone()).collect();
    assert_eq!(data, vec!["one", "two", "three"]);

    // The callback must have fired progressively: the first event cannot have
    // arrived at the same time as the last if they were streamed.
    let seen = timings.lock().unwrap();
    assert_eq!(seen.len(), 3);
    let first = seen[0].1;
    let last = seen[2].1;
    assert!(
        last > first + Duration::from_millis(40),
        "events arrived together ({first:?} then {last:?}) — the body was buffered"
    );
}

#[tokio::test]
async fn the_event_limit_stops_the_stream_early() {
    let addr = serve(
        STREAM_HEAD,
        vec!["data: a\n\n", "data: b\n\n", "data: c\n\n", "data: d\n\n"],
        Duration::from_millis(20),
    )
    .await;

    let limits = StreamLimits {
        max_events: 2,
        max_duration: Duration::from_secs(10),
    };
    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);

    let outcome = executor
        .execute_stream(&spec(addr), &mut ctx, &limits, |_| {})
        .await
        .expect("stream");

    assert_eq!(outcome.ended, StreamEnd::LimitReached);
    assert_eq!(outcome.events.len(), 2);
}

#[tokio::test]
async fn a_stream_that_goes_quiet_times_out_rather_than_hanging() {
    // One event, then silence, and the server never closes.
    let addr = serve(
        STREAM_HEAD,
        vec!["data: only\n\n", "data: never-sent\n\n"],
        Duration::from_secs(30),
    )
    .await;

    let limits = StreamLimits {
        max_events: 100,
        max_duration: Duration::from_millis(400),
    };
    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);

    let started = std::time::Instant::now();
    let outcome = executor
        .execute_stream(&spec(addr), &mut ctx, &limits, |_| {})
        .await
        .expect("stream");

    assert_eq!(outcome.ended, StreamEnd::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the timeout was not honoured"
    );
}

/// Regression: a healthy stream must outlive the request timeout.
///
/// `settings.timeout_ms` bounds an ordinary request end to end. Applied to a
/// stream it killed the connection mid-flight and surfaced as a transport error
/// rather than a clean stop — which is what happened the first time this was
/// run against a real server that held the connection open.
#[tokio::test]
async fn a_stream_is_not_cut_short_by_the_request_timeout() {
    let addr = serve(
        STREAM_HEAD,
        vec!["data: one\n\n", "data: two\n\n", "data: three\n\n"],
        Duration::from_millis(120),
    )
    .await;

    let mut spec = spec(addr);
    // Far shorter than the stream takes to deliver everything.
    spec.settings.timeout_ms = 200;

    let limits = StreamLimits {
        max_events: 10,
        max_duration: Duration::from_secs(5),
    };
    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);

    let outcome = executor
        .execute_stream(&spec, &mut ctx, &limits, |_| {})
        .await
        .expect("the request timeout must not abort a live stream");

    assert_eq!(outcome.events.len(), 3, "the stream was cut short");
    assert_eq!(outcome.ended, StreamEnd::Closed);
}

/// An endpoint that returns JSON instead of a stream — a 500 page, or an auth
/// error — must be reported at once, not waited on.
#[tokio::test]
async fn a_non_stream_response_is_reported_immediately() {
    let head = "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 26\r\nConnection: close\r\n\r\n{\"error\":\"invalid_token\"}\n";
    let addr = serve(head, vec![], Duration::from_millis(10)).await;

    let limits = StreamLimits {
        max_events: 100,
        max_duration: Duration::from_secs(30),
    };
    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);

    let started = std::time::Instant::now();
    let outcome = executor
        .execute_stream(&spec(addr), &mut ctx, &limits, |_| {})
        .await
        .expect("request");

    assert_eq!(outcome.status, 401);
    assert_eq!(outcome.ended, StreamEnd::NotAStream);
    assert!(outcome.events.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "it waited on a non-stream"
    );
}

#[tokio::test]
async fn named_events_and_json_payloads_survive_the_round_trip() {
    let addr = serve(
        STREAM_HEAD,
        vec![
            "event: token\ndata: {\"delta\":\"Hel\"}\n\n",
            "event: token\ndata: {\"delta\":\"lo\"}\n\n",
            "event: done\ndata: [DONE]\n\n",
        ],
        Duration::from_millis(10),
    )
    .await;

    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);

    let outcome = executor
        .execute_stream(&spec(addr), &mut ctx, &StreamLimits::default(), |_| {})
        .await
        .expect("stream");

    assert_eq!(outcome.events.len(), 3);
    assert_eq!(outcome.events[0].name.as_deref(), Some("token"));
    assert_eq!(
        outcome.events[0].json().unwrap()["delta"],
        serde_json::json!("Hel")
    );
    assert_eq!(outcome.events[2].data, "[DONE]");
}

/// The Accept header is what tells the server to stream at all, so it must be
/// sent — unless the request set its own.
#[tokio::test]
async fn an_event_stream_accept_header_is_sent_by_default() {
    let addr = serve(STREAM_HEAD, vec!["data: x\n\n"], Duration::from_millis(5)).await;

    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut ctx = RunContext::new(&mut resolver, &mut sessions);

    let outcome = executor
        .execute_stream(&spec(addr), &mut ctx, &StreamLimits::default(), |_| {})
        .await
        .expect("stream");

    let accept = outcome
        .sent
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("accept"))
        .map(|(_, value)| value.clone());
    assert_eq!(accept.as_deref(), Some("text/event-stream"));

    // And a request with its own Accept keeps it.
    let mut custom = spec(addr);
    custom.headers.set("Accept", "application/x-ndjson");
    let outcome = executor
        .execute_stream(&custom, &mut ctx, &StreamLimits::default(), |_| {})
        .await
        .expect("stream");
    let accepts: Vec<&String> = outcome
        .sent
        .headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("accept"))
        .map(|(_, v)| v)
        .collect();
    assert_eq!(
        accepts,
        vec!["application/x-ndjson"],
        "the request's own Accept was overridden"
    );
}
