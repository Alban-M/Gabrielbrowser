//! A local origin server, so measurements are of Gabriel rather than of the
//! internet. Every number in the report is against this.

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use tokio::net::TcpListener;

pub async fn spawn() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service_fn(route))
                    .await;
            });
        }
    });

    addr
}

async fn route(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let path = req.uri().path().to_string();

    let response = match path.as_str() {
        // A small JSON document, the shape of a typical API response.
        "/json" => json_response(
            r#"{"id":7,"name":"widget","tags":["a","b"],"meta":{"ok":true,"count":3}}"#,
        ),
        "/cookie" => Response::builder()
            .header("content-type", "application/json")
            .header("set-cookie", "session_id=bench-session; Path=/")
            .body(Full::new(Bytes::from_static(b"{\"ok\":true}")))
            .expect("builds"),
        path if path.starts_with("/bytes/") => {
            let size: usize = path.trim_start_matches("/bytes/").parse().unwrap_or(1024);
            Response::builder()
                .header("content-type", "application/octet-stream")
                .body(Full::new(Bytes::from(vec![b'x'; size])))
                .expect("builds")
        }
        _ => json_response(r#"{"ok":true}"#),
    };

    Ok(response)
}

fn json_response(body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .expect("builds")
}
