//! mTLS, against a server that actually demands a client certificate.
//!
//! `settings.client_cert` used to parse and then do nothing, which is the worst
//! kind of feature: the request went out without an identity and nothing said
//! so. A test that only checked the plumbing would not have caught that, so this
//! one completes a real handshake with a server configured to reject anyone who
//! cannot present a certificate signed by its CA.

use gabriel_core::model::RequestSpec;
use gabriel_core::vars::Resolver;
use gabriel_engine::session::SessionStore;
use gabriel_engine::{Executor, RunContext};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Authority {
    params: CertificateParams,
    key: KeyPair,
    cert_der: CertificateDer<'static>,
}

fn authority() -> Authority {
    let key = KeyPair::generate().expect("ca key");
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Gabriel mTLS test CA");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];

    let cert = params.self_signed(&key).expect("self-signed ca");
    let cert_der = cert.der().clone();
    Authority {
        params,
        key,
        cert_der,
    }
}

/// A leaf certificate signed by the CA, returned as (DER cert, DER key, PEM bundle).
fn leaf(
    ca: &Authority,
    common_name: &str,
    sans: Vec<String>,
    purpose: ExtendedKeyUsagePurpose,
) -> (CertificateDer<'static>, PrivateKeyDer<'static>, String) {
    let key = KeyPair::generate().expect("leaf key");
    let mut params = CertificateParams::new(sans).expect("leaf params");
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    params.distinguished_name = dn;
    params.extended_key_usages = vec![purpose];

    let issuer = Issuer::from_params(&ca.params, &ca.key);
    let cert = params.signed_by(&key, &issuer).expect("signed leaf");

    let pem = format!("{}{}", key.serialize_pem(), cert.pem());
    let key_der = PrivateKeyDer::try_from(key.serialize_der()).expect("key der");
    (cert.der().clone(), key_der, pem)
}

/// Serve one TLS connection that requires a client certificate signed by `ca`.
/// Returns the address, and a receiver that reports whether the handshake
/// succeeded.
async fn serve_mtls(ca: &Authority) -> (SocketAddr, tokio::sync::mpsc::UnboundedReceiver<bool>) {
    let (server_cert, server_key, _) = leaf(
        ca,
        "localhost",
        vec!["localhost".to_string(), "127.0.0.1".to_string()],
        ExtendedKeyUsagePurpose::ServerAuth,
    );

    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.cert_der.clone()).expect("trust the test ca");
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .expect("client verifier");

    let config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![server_cert, ca.cert_der.clone()], server_key)
        .expect("server config");

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(async move {
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let acceptor = acceptor.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                match acceptor.accept(stream).await {
                    Ok(mut tls) => {
                        let _ = tx.send(true);
                        let mut buffer = vec![0u8; 4096];
                        let _ = tls.read(&mut buffer).await;

                        // The length must match the body exactly, and the TLS
                        // session must be shut down cleanly — a client is right
                        // to complain about a truncated stream, and that
                        // complaint would otherwise look like a Gabriel bug.
                        let body = r#"{"mtls":"ok"}"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = tls.write_all(response.as_bytes()).await;
                        let _ = tls.flush().await;
                        let _ = tls.shutdown().await;
                    }
                    // A client with no certificate is rejected here.
                    Err(_) => {
                        let _ = tx.send(false);
                    }
                }
            });
        }
    });

    (addr, rx)
}

fn spec_for(addr: SocketAddr) -> RequestSpec {
    let mut spec = RequestSpec::new("GET", format!("https://localhost:{}/", addr.port()));
    // The test CA is not in any trust store; server verification is not what is
    // under test here, the client certificate is.
    spec.settings.verify_tls = false;
    spec
}

#[tokio::test]
async fn a_request_presents_a_client_certificate_from_the_vault() {
    let ca = authority();
    let (addr, mut handshakes) = serve_mtls(&ca).await;
    let (_, _, client_pem) = leaf(
        &ca,
        "gabriel-client",
        vec!["gabriel-client".to_string()],
        ExtendedKeyUsagePurpose::ClientAuth,
    );

    let mut spec = spec_for(addr);
    spec.settings.client_cert = Some("{{secret:client_identity}}".to_string());

    let secrets: std::collections::BTreeMap<String, String> =
        [("client_identity".to_string(), client_pem)].into();
    let mut resolver = Resolver::new().with_secrets(&secrets);
    let mut sessions = SessionStore::new();
    let mut executor = Executor::new();

    let outcome = {
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);
        executor.execute(&spec, &mut ctx).await
    };

    let outcome = outcome.expect("the handshake should succeed with a client certificate");
    assert_eq!(outcome.response.status, 200);
    assert!(outcome.response.text().contains("mtls"));
    assert_eq!(handshakes.recv().await, Some(true));
}

#[tokio::test]
async fn the_same_request_without_a_certificate_is_rejected() {
    let ca = authority();
    let (addr, mut handshakes) = serve_mtls(&ca).await;

    let spec = spec_for(addr);
    assert!(spec.settings.client_cert.is_none());

    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut executor = Executor::new();

    let result = {
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);
        executor.execute(&spec, &mut ctx).await
    };

    assert!(
        result.is_err(),
        "a server requiring mTLS accepted a request with no certificate — \
         which would mean the setting is doing nothing"
    );
    assert_eq!(handshakes.recv().await, Some(false));
}

#[tokio::test]
async fn a_certificate_read_from_a_file_works_too() {
    let ca = authority();
    let (addr, _handshakes) = serve_mtls(&ca).await;
    let (_, _, client_pem) = leaf(
        &ca,
        "gabriel-client",
        vec!["gabriel-client".to_string()],
        ExtendedKeyUsagePurpose::ClientAuth,
    );

    let dir = std::env::temp_dir().join(format!("gabriel-mtls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("client.pem");
    std::fs::write(&path, client_pem).expect("write pem");

    let mut spec = spec_for(addr);
    spec.settings.client_cert = Some("client.pem".to_string());

    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();
    let mut executor = Executor::new();

    let outcome = {
        // Paths resolve relative to the collection root.
        let mut ctx = RunContext::new(&mut resolver, &mut sessions).with_base_dir(&dir);
        executor.execute(&spec, &mut ctx).await
    };

    assert_eq!(
        outcome
            .expect("handshake with a file-based cert")
            .response
            .status,
        200
    );
}

#[tokio::test]
async fn a_malformed_certificate_is_reported_rather_than_ignored() {
    let ca = authority();
    let (addr, _handshakes) = serve_mtls(&ca).await;

    let mut spec = spec_for(addr);
    spec.settings.client_cert = Some("{{secret:broken}}".to_string());

    let secrets: std::collections::BTreeMap<String, String> = [(
        "broken".to_string(),
        "-----BEGIN CERTIFICATE-----\nnot base64 at all\n-----END CERTIFICATE-----".to_string(),
    )]
    .into();
    let mut resolver = Resolver::new().with_secrets(&secrets);
    let mut sessions = SessionStore::new();
    let mut executor = Executor::new();

    let error = {
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);
        executor
            .execute(&spec, &mut ctx)
            .await
            .expect_err("should refuse to continue")
    };
    let message = error.to_string();
    assert!(
        message.contains("client certificate"),
        "the error should name the problem: {message}"
    );
}
