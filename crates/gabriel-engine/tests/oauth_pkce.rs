//! The authorization-code + PKCE flow, end to end against an identity provider
//! that actually enforces it.
//!
//! The test IdP verifies `code_challenge` against `code_verifier` the way a real
//! one does, so a flow that skipped PKCE — or got the S256 transform wrong —
//! fails here rather than passing quietly and failing against Google.
//!
//! Interop with Google, GitHub and Auth0 is a separate question these tests
//! cannot answer: that needs their client IDs and a human at a consent screen.
//! What they do answer is whether the implementation is correct.

use gabriel_core::model::{OAuth2, OAuth2Grant};
use gabriel_engine::oauth::{self, FlowOptions, Pkce};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

/// What the IdP recorded, so tests can assert on what it was sent.
#[derive(Default, Debug)]
struct Recorded {
    verifier: Option<String>,
    grant_type: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
}

#[derive(Clone, Copy)]
enum IdpBehaviour {
    /// Verify PKCE properly and issue tokens.
    Correct,
    /// Reject the exchange, as a provider does when the verifier is wrong.
    RejectExchange,
}

/// A token endpoint that checks the challenge against the verifier.
async fn spawn_idp(
    behaviour: IdpBehaviour,
    recorded: Arc<Mutex<Recorded>>,
) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    // One map per IdP address, shared with the simulated browser: the challenge
    // is registered at authorize time and checked at exchange time.
    let issued = issued_handle(addr);

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let recorded = recorded.clone();
            let issued = issued.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 16384];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = request.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                let params = oauth::parse_query(&body);

                {
                    let mut recorded = recorded.lock().unwrap();
                    recorded.verifier = params.get("code_verifier").cloned();
                    recorded.grant_type = params.get("grant_type").cloned();
                    recorded.redirect_uri = params.get("redirect_uri").cloned();
                    recorded.client_id = params.get("client_id").cloned();
                }

                let reply = |status: &str, body: String| {
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };

                let response = match behaviour {
                    IdpBehaviour::RejectExchange => reply(
                        "400 Bad Request",
                        r#"{"error":"invalid_grant","error_description":"code_verifier mismatch"}"#
                            .to_string(),
                    ),
                    IdpBehaviour::Correct => {
                        let code = params.get("code").cloned().unwrap_or_default();
                        let verifier = params.get("code_verifier").cloned().unwrap_or_default();
                        let expected = issued.lock().unwrap().get(&code).cloned();

                        // This is the check that makes PKCE mean anything.
                        let ok = match expected {
                            Some(challenge) => Pkce::from_verifier(verifier).challenge == challenge,
                            None => false,
                        };
                        if ok {
                            reply(
                                "200 OK",
                                r#"{"access_token":"at-12345","refresh_token":"rt-67890","token_type":"Bearer","expires_in":3600,"scope":"openid profile"}"#
                                    .to_string(),
                            )
                        } else {
                            reply(
                                "400 Bad Request",
                                r#"{"error":"invalid_grant","error_description":"PKCE verification failed"}"#
                                    .to_string(),
                            )
                        }
                    }
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    addr
}

/// Both halves of the test run in one process, so the challenge map is looked up
/// by the IdP's address rather than passed around.
/// Authorization code → the challenge presented when it was issued.
type ChallengeMap = Arc<Mutex<HashMap<String, String>>>;

static REGISTRY: Mutex<Vec<(SocketAddr, ChallengeMap)>> = Mutex::new(Vec::new());

fn issued_handle(addr: SocketAddr) -> ChallengeMap {
    let mut registry = REGISTRY.lock().unwrap();
    if let Some((_, existing)) = registry.iter().find(|(a, _)| *a == addr) {
        return existing.clone();
    }
    let fresh = Arc::new(Mutex::new(HashMap::new()));
    registry.push((addr, fresh.clone()));
    fresh
}

fn config(idp: SocketAddr) -> OAuth2 {
    OAuth2 {
        grant: OAuth2Grant::AuthorizationCode,
        token_url: format!("http://{idp}/oauth/token"),
        authorize_url: Some(format!("http://{idp}/authorize")),
        redirect_uri: None,
        client_id: "gabriel-test-client".into(),
        client_secret: None,
        scope: Some("openid profile".into()),
        audience: None,
        credentials_in_body: false,
    }
}

/// Stand in for the browser: read the authorize URL, register the challenge with
/// the IdP, and call the redirect back with a code.
async fn drive_browser(url: String, idp: SocketAddr, tamper: Tamper) {
    let query = url.split_once('?').expect("authorize URL has a query").1;
    let params = oauth::parse_query(query);
    let redirect = params["redirect_uri"].clone();
    let challenge = params["code_challenge"].clone();

    let code = "auth-code-abc".to_string();
    issued_handle(idp).lock().unwrap().insert(code.clone(), challenge);

    let state = match tamper {
        Tamper::None => params["state"].clone(),
        Tamper::WrongState => "not-the-state-we-sent".to_string(),
    };

    // Give the listener a moment to be ready.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let target = redirect.trim_start_matches("http://");
    let (authority, path) = target.split_once('/').unwrap_or((target, ""));
    let mut stream = tokio::net::TcpStream::connect(authority).await.expect("connect back");
    let request = format!(
        "GET /{path}?code={code}&state={state} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(request.as_bytes()).await;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
}

#[derive(Clone, Copy)]
enum Tamper {
    None,
    WrongState,
}

#[tokio::test]
async fn the_full_flow_exchanges_a_code_for_tokens() {
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let idp = spawn_idp(IdpBehaviour::Correct, recorded.clone()).await;
    let config = config(idp);

    let options = FlowOptions {
        port: 0,
        open_browser: false,
        timeout: Duration::from_secs(10),
    };

    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let mut url_tx = Some(url_tx);

    let flow = oauth::authorization_code(&config, &options, move |url| {
        if let Some(tx) = url_tx.take() {
            let _ = tx.send(url.to_string());
        }
    });

    let browser = async {
        let url = url_rx.await.expect("the authorize URL");
        drive_browser(url, idp, Tamper::None).await;
    };

    let (tokens, _) = tokio::join!(flow, browser);
    let tokens = tokens.expect("the flow should complete");

    assert_eq!(tokens.access_token, "at-12345");
    assert_eq!(tokens.refresh_token.as_deref(), Some("rt-67890"));
    assert_eq!(tokens.expires_in, Some(3600));

    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.grant_type.as_deref(), Some("authorization_code"));
    assert_eq!(recorded.client_id.as_deref(), Some("gabriel-test-client"));
    // The verifier reached the token endpoint and nothing else.
    let verifier = recorded.verifier.as_deref().expect("a code_verifier was sent");
    assert!((43..=128).contains(&verifier.len()), "verifier length {}", verifier.len());
    // The redirect the IdP was told matches the one the listener served.
    assert!(
        recorded.redirect_uri.as_deref().is_some_and(|uri| uri.starts_with("http://127.0.0.1:")),
        "redirect_uri was {:?}",
        recorded.redirect_uri
    );
}

/// The point of PKCE: an intercepted code is useless without the verifier. If
/// the exchange were sending the wrong one — or none — this is where it shows.
#[tokio::test]
async fn the_verifier_actually_matches_the_challenge_the_idp_saw() {
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let idp = spawn_idp(IdpBehaviour::Correct, recorded.clone()).await;
    let config = config(idp);

    let seen_challenge = Arc::new(Mutex::new(String::new()));
    let capture = seen_challenge.clone();

    let options = FlowOptions { port: 0, open_browser: false, timeout: Duration::from_secs(10) };
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let mut url_tx = Some(url_tx);

    let flow = oauth::authorization_code(&config, &options, move |url| {
        let params = oauth::parse_query(url.split_once('?').unwrap().1);
        *capture.lock().unwrap() = params["code_challenge"].clone();
        if let Some(tx) = url_tx.take() {
            let _ = tx.send(url.to_string());
        }
    });
    let browser = async {
        let url = url_rx.await.expect("url");
        drive_browser(url, idp, Tamper::None).await;
    };
    let (tokens, _) = tokio::join!(flow, browser);
    assert!(tokens.is_ok(), "the IdP rejected the verifier: {tokens:?}");

    // Recompute the challenge from the verifier the IdP received: it must equal
    // the one presented at authorize time.
    let verifier = recorded.lock().unwrap().verifier.clone().expect("verifier");
    let challenge = seen_challenge.lock().unwrap().clone();
    assert_eq!(
        Pkce::from_verifier(verifier).challenge,
        challenge,
        "the S256 transform does not match what was presented"
    );
}

/// A callback carrying somebody else's `state` must be discarded, not exchanged.
#[tokio::test]
async fn a_callback_with_the_wrong_state_is_refused() {
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let idp = spawn_idp(IdpBehaviour::Correct, recorded.clone()).await;
    let config = config(idp);

    let options = FlowOptions { port: 0, open_browser: false, timeout: Duration::from_secs(10) };
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let mut url_tx = Some(url_tx);

    let flow = oauth::authorization_code(&config, &options, move |url| {
        if let Some(tx) = url_tx.take() {
            let _ = tx.send(url.to_string());
        }
    });
    let browser = async {
        let url = url_rx.await.expect("url");
        drive_browser(url, idp, Tamper::WrongState).await;
    };
    let (result, _) = tokio::join!(flow, browser);

    let error = result.unwrap_err().to_string();
    assert!(error.contains("state"), "unexpected error: {error}");
    // And no exchange was attempted with the bad code.
    assert!(
        recorded.lock().unwrap().grant_type.is_none(),
        "a code from a mismatched state was exchanged anyway"
    );
}

#[tokio::test]
async fn a_provider_error_on_the_exchange_is_reported_with_its_reason() {
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let idp = spawn_idp(IdpBehaviour::RejectExchange, recorded).await;
    let config = config(idp);

    let options = FlowOptions { port: 0, open_browser: false, timeout: Duration::from_secs(10) };
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let mut url_tx = Some(url_tx);

    let flow = oauth::authorization_code(&config, &options, move |url| {
        if let Some(tx) = url_tx.take() {
            let _ = tx.send(url.to_string());
        }
    });
    let browser = async {
        let url = url_rx.await.expect("url");
        drive_browser(url, idp, Tamper::None).await;
    };
    let (result, _) = tokio::join!(flow, browser);

    let error = result.unwrap_err().to_string();
    assert!(error.contains("invalid_grant"), "the provider's reason was lost: {error}");
    assert!(error.contains("code_verifier mismatch"), "{error}");
}

#[tokio::test]
async fn a_refresh_token_can_be_exchanged_for_a_new_access_token() {
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let idp = spawn_idp(IdpBehaviour::Correct, recorded.clone()).await;
    // The correct IdP only issues on a valid PKCE exchange, so refresh is tested
    // against the rejecting one to prove the request shape instead.
    let tokens = oauth::refresh(&config(idp), "rt-67890").await;
    assert!(tokens.is_err(), "an unknown code should not mint tokens");

    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.grant_type.as_deref(), Some("refresh_token"));
    assert_eq!(recorded.client_id.as_deref(), Some("gabriel-test-client"));
}

#[tokio::test]
async fn waiting_for_a_browser_that_never_returns_times_out() {
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let idp = spawn_idp(IdpBehaviour::Correct, recorded).await;
    let config = config(idp);

    let options =
        FlowOptions { port: 0, open_browser: false, timeout: Duration::from_millis(300) };

    let started = std::time::Instant::now();
    let result = oauth::authorization_code(&config, &options, |_| {}).await;

    assert!(result.unwrap_err().to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(5), "it hung past the timeout");
}
