//! Redirect handling, against a real socket.
//!
//! These live in an integration test rather than a unit test on purpose: the
//! bug they cover — a cookie set on a 302 vanishing — was invisible to unit
//! tests because it lived in the interaction between the HTTP client's own
//! redirect follower and our session store. Only a real request chain shows it.

use gabriel_core::model::{Auth, Body, RequestSpec};
use gabriel_core::vars::Resolver;
use gabriel_engine::session::SessionStore;
use gabriel_engine::{Executor, RunContext};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One scripted exchange: what the server should reply to a given path.
struct Route {
    path: &'static str,
    response: String,
}

/// A minimal HTTP server built on raw sockets — no framework, so the exact
/// bytes on the wire are what the test says they are. Every response closes the
/// connection, which keeps the reader trivial.
async fn serve(routes: Vec<Route>) -> (SocketAddr, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let routes: Vec<(&str, String)> =
                routes.iter().map(|r| (r.path, r.response.clone())).collect();
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8192];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();

                // Hand the whole request text back to the test so it can assert
                // on what actually arrived.
                let _ = tx.send(request);

                let response = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, r)| r.clone())
                    .unwrap_or_else(|| {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    });
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    (addr, rx)
}

fn body_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn redirect_response(status: &str, location: &str, set_cookie: Option<&str>) -> String {
    let cookie = set_cookie
        .map(|c| format!("Set-Cookie: {c}\r\n"))
        .unwrap_or_default();
    format!(
        "HTTP/1.1 {status}\r\nLocation: {location}\r\n{cookie}Content-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// Header names on the wire are lowercase, so assertions compare
/// case-insensitively rather than guessing at the client's capitalisation.
fn has_header(request: &str, name: &str, value: &str) -> bool {
    request
        .to_ascii_lowercase()
        .contains(&format!("{}: {}", name.to_ascii_lowercase(), value.to_ascii_lowercase()))
}

fn has_header_name(request: &str, name: &str) -> bool {
    request
        .to_ascii_lowercase()
        .lines()
        .any(|line| line.starts_with(&format!("{}:", name.to_ascii_lowercase())))
}

async fn run(spec: &RequestSpec, sessions: &mut SessionStore) -> gabriel_engine::RunOutcome {
    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    let mut ctx = RunContext::new(&mut resolver, sessions);
    executor.execute(spec, &mut ctx).await.expect("execute")
}

/// The regression this whole change exists for.
#[tokio::test]
async fn a_cookie_set_on_a_redirect_reaches_the_redirect_target() {
    let (addr, mut requests) = serve(vec![
        Route {
            path: "/login",
            response: redirect_response("302 Found", "/home", Some("sid=from-redirect; Path=/")),
        },
        Route { path: "/home", response: body_response(r#"{"ok":true}"#) },
    ])
    .await;

    let mut sessions = SessionStore::new();
    let outcome = run(
        &RequestSpec::new("GET", format!("http://{addr}/login")),
        &mut sessions,
    )
    .await;

    assert_eq!(outcome.response.status, 200, "should have followed to /home");
    assert_eq!(outcome.redirects.len(), 1);
    assert_eq!(outcome.redirects[0].status, 302);

    let first = requests.recv().await.expect("login request");
    let second = requests.recv().await.expect("home request");
    assert!(!has_header_name(&first, "cookie"), "no cookie exists yet on the first hop");
    assert!(
        has_header(&second, "cookie", "sid=from-redirect"),
        "the cookie set on the 302 never reached /home:\n{second}"
    );
}

#[tokio::test]
async fn a_cookie_set_on_a_redirect_is_recorded_in_the_session() {
    let (addr, _requests) = serve(vec![
        Route {
            path: "/login",
            response: redirect_response("302 Found", "/home", Some("sid=persisted; Path=/")),
        },
        Route { path: "/home", response: body_response(r#"{"ok":true}"#) },
    ])
    .await;

    let mut sessions = SessionStore::new();
    run(&RequestSpec::new("GET", format!("http://{addr}/login")), &mut sessions).await;

    let host = "127.0.0.1".to_string();
    assert_eq!(
        sessions.cookie_header("default", &host, "/", false).as_deref(),
        Some("sid=persisted"),
        "the session store learned nothing from the redirect"
    );
}

#[tokio::test]
async fn credentials_do_not_follow_a_redirect_to_another_origin() {
    // The second server stands in for wherever an open redirect might point.
    let (elsewhere, mut elsewhere_requests) =
        serve(vec![Route { path: "/taken", response: body_response(r#"{"ok":true}"#) }]).await;
    let (addr, _requests) = serve(vec![Route {
        path: "/start",
        response: redirect_response(
            "302 Found",
            &format!("http://127.0.0.1:{}/taken", elsewhere.port()),
            None,
        ),
    }])
    .await;

    let mut spec = RequestSpec::new("GET", format!("http://127.0.0.1:{}/start", addr.port()));
    spec.auth = Some(Auth::Bearer { token: "sk-live-should-not-travel".into() });
    spec.headers.set("X-Trace", "kept");

    let mut sessions = SessionStore::new();
    let outcome = run(&spec, &mut sessions).await;
    assert_eq!(outcome.response.status, 200);

    let received = elsewhere_requests.recv().await.expect("request to the other origin");
    assert!(
        !received.contains("sk-live-should-not-travel"),
        "the bearer token leaked across origins:\n{received}"
    );
    assert!(
        has_header(&received, "x-trace", "kept"),
        "ordinary headers should still travel:\n{received}"
    );
}

#[tokio::test]
async fn a_303_turns_a_post_into_a_get_and_drops_the_body() {
    let (addr, mut requests) = serve(vec![
        Route { path: "/submit", response: redirect_response("303 See Other", "/done", None) },
        Route { path: "/done", response: body_response(r#"{"ok":true}"#) },
    ])
    .await;

    let mut spec = RequestSpec::new("POST", format!("http://{addr}/submit"));
    spec.body = Some(Body::Json { content: r#"{"payload":"original"}"#.into() });

    let mut sessions = SessionStore::new();
    let outcome = run(&spec, &mut sessions).await;
    assert_eq!(outcome.response.status, 200);

    let _first = requests.recv().await.expect("submit");
    let second = requests.recv().await.expect("done");
    assert!(second.starts_with("GET /done"), "303 must become a GET:\n{second}");
    assert!(!second.contains("original"), "the body should not be resent:\n{second}");
    assert!(
        !has_header_name(&second, "content-type"),
        "a GET with no body should not claim a content type:\n{second}"
    );
}

#[tokio::test]
async fn a_307_preserves_the_method_and_the_body() {
    let (addr, mut requests) = serve(vec![
        Route {
            path: "/submit",
            response: redirect_response("307 Temporary Redirect", "/done", None),
        },
        Route { path: "/done", response: body_response(r#"{"ok":true}"#) },
    ])
    .await;

    let mut spec = RequestSpec::new("POST", format!("http://{addr}/submit"));
    spec.body = Some(Body::Json { content: r#"{"payload":"original"}"#.into() });

    let mut sessions = SessionStore::new();
    run(&spec, &mut sessions).await;

    let _first = requests.recv().await.expect("submit");
    let second = requests.recv().await.expect("done");
    assert!(second.starts_with("POST /done"), "307 must keep the method:\n{second}");
    assert!(second.contains("original"), "307 must resend the body:\n{second}");
}

#[tokio::test]
async fn redirects_can_be_switched_off() {
    let (addr, _requests) = serve(vec![Route {
        path: "/login",
        response: redirect_response("302 Found", "/home", None),
    }])
    .await;

    let mut spec = RequestSpec::new("GET", format!("http://{addr}/login"));
    spec.settings.follow_redirects = false;

    let mut sessions = SessionStore::new();
    let outcome = run(&spec, &mut sessions).await;
    assert_eq!(outcome.response.status, 302, "the 302 itself should be returned");
    assert!(outcome.redirects.is_empty());
}

#[tokio::test]
async fn a_redirect_loop_is_bounded() {
    let (addr, _requests) = serve(vec![Route {
        path: "/loop",
        response: redirect_response("302 Found", "/loop", None),
    }])
    .await;

    let mut spec = RequestSpec::new("GET", format!("http://{addr}/loop"));
    spec.settings.max_redirects = 3;

    let mut sessions = SessionStore::new();
    let outcome = run(&spec, &mut sessions).await;
    assert_eq!(outcome.redirects.len(), 3, "should stop at the limit");
    assert_eq!(outcome.response.status, 302, "and return the last redirect itself");
}

#[tokio::test]
async fn a_relative_location_resolves_against_the_current_url() {
    let (addr, mut requests) = serve(vec![
        Route {
            path: "/a/b/start",
            response: redirect_response("302 Found", "../target", None),
        },
        Route { path: "/a/target", response: body_response(r#"{"ok":true}"#) },
    ])
    .await;

    let mut sessions = SessionStore::new();
    let outcome = run(
        &RequestSpec::new("GET", format!("http://{addr}/a/b/start")),
        &mut sessions,
    )
    .await;

    assert_eq!(outcome.response.status, 200, "relative Location was not resolved");
    let _first = requests.recv().await.expect("start");
    let second = requests.recv().await.expect("target");
    assert!(second.starts_with("GET /a/target"), "{second}");
}

#[tokio::test]
async fn an_inherited_session_cookie_is_sent_on_every_hop() {
    let (addr, mut requests) = serve(vec![
        Route { path: "/one", response: redirect_response("302 Found", "/two", None) },
        Route { path: "/two", response: body_response(r#"{"ok":true}"#) },
    ])
    .await;

    let mut sessions = SessionStore::new();
    sessions.record_set_cookies("work", ["sid=pre-existing; Path=/"], "127.0.0.1", "/");

    let mut spec = RequestSpec::new("GET", format!("http://{addr}/one"));
    spec.auth = Some(Auth::Session { session: Some("work".into()) });

    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    {
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);
        executor.execute(&spec, &mut ctx).await.expect("execute");
    }

    let first = requests.recv().await.expect("one");
    let second = requests.recv().await.expect("two");
    assert!(has_header(&first, "cookie", "sid=pre-existing"), "{first}");
    assert!(has_header(&second, "cookie", "sid=pre-existing"), "{second}");
}
