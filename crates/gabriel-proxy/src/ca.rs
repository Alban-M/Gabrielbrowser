//! The interception certificate authority.
//!
//! Decrypting TLS is the sharpest tool in the product, so the handling of the
//! key is deliberately conservative:
//!
//! * the CA is **generated per install** and never shipped — a shared MITM root
//!   in a distributed binary would be a vulnerability, not a feature;
//! * the private key is written `0600` and stays in the collection's runtime
//!   directory, which is gitignored;
//! * leaf certificates are minted in memory, per host, and never persisted.
//!
//! Trusting this CA is an explicit act by the developer (`gabriel ca install`
//! prints what to do); nothing here installs itself into a trust store.

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, date_time_ymd,
};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const CA_CERT_FILE: &str = "gabriel-ca.pem";
pub const CA_KEY_FILE: &str = "gabriel-ca.key";

/// Browsers reject long-lived leaves from publicly trusted roots; local roots
/// are exempt, but staying under the limit avoids arguing with the exceptions.
const LEAF_VALIDITY_DAYS: i64 = 300;
const CA_VALIDITY_YEARS: i64 = 5;

#[derive(Debug, thiserror::Error)]
pub enum CaError {
    #[error("{path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("certificate generation failed: {0}")]
    Rcgen(#[from] rcgen::Error),

    #[error("{path} does not contain a certificate")]
    NoCertificate { path: PathBuf },

    #[error("TLS configuration failed: {0}")]
    Tls(#[from] rustls::Error),
}

type Result<T> = std::result::Result<T, CaError>;

pub struct CertificateAuthority {
    cert_path: PathBuf,
    cert_der: CertificateDer<'static>,
    cert_pem: String,
    issuer: Issuer<'static, KeyPair>,
    /// Per-host TLS configs, minted on first use and kept for the session.
    cache: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl CertificateAuthority {
    /// Load the CA from `dir`, generating one on first use.
    pub fn load_or_create(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);

        if !(cert_path.exists() && key_path.exists()) {
            return Self::create(dir);
        }

        let cert_pem = read(&cert_path)?;
        let key_pem = read(&key_path)?;
        let key = KeyPair::from_pem(&key_pem)?;
        let cert_der = first_certificate(&cert_pem, &cert_path)?;

        Ok(CertificateAuthority {
            cert_path,
            cert_der,
            cert_pem,
            issuer: Issuer::new(ca_params()?, key),
            cache: Mutex::new(HashMap::new()),
        })
    }

    fn create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|source| CaError::Io {
            path: dir.to_path_buf(),
            source,
        })?;

        let key = KeyPair::generate()?;
        let params = ca_params()?;
        let cert = params.self_signed(&key)?;

        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);
        let cert_pem = cert.pem();

        write(&cert_path, cert_pem.as_bytes(), 0o644)?;
        write(&key_path, key.serialize_pem().as_bytes(), 0o600)?;

        Ok(CertificateAuthority {
            cert_path,
            cert_der: cert.der().clone(),
            cert_pem,
            issuer: Issuer::new(params, key),
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn cert_path(&self) -> &Path {
        &self.cert_path
    }

    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// A TLS server configuration presenting a certificate for `host`.
    pub fn server_config(&self, host: &str) -> Result<Arc<ServerConfig>> {
        if let Some(config) = self.cache.lock().expect("cache lock").get(host) {
            return Ok(config.clone());
        }

        let (leaf, key) = self.mint_leaf(host)?;
        let chain = vec![leaf, self.cert_der.clone()];

        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
        // The client is a browser on this machine; ALPN must offer what it
        // expects or the handshake fails after the CONNECT succeeds.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];

        let config = Arc::new(config);
        self.cache
            .lock()
            .expect("cache lock")
            .insert(host.to_string(), config.clone());
        Ok(config)
    }

    fn mint_leaf(&self, host: &str) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
        let key = KeyPair::generate()?;

        let mut params = CertificateParams::new(vec![host.to_string()])?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host);
        params.distinguished_name = dn;
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];

        let now = gabriel_core::now_ms();
        let (y0, m0, d0) = gabriel_core::date_parts(now, -1);
        let (y1, m1, d1) = gabriel_core::date_parts(now, LEAF_VALIDITY_DAYS);
        params.not_before = date_time_ymd(y0 as i32, m0 as u8, d0 as u8);
        params.not_after = date_time_ymd(y1 as i32, m1 as u8, d1 as u8);

        let cert = params.signed_by(&key, &self.issuer)?;
        let key_der = PrivateKeyDer::try_from(key.serialize_der())
            .map_err(|e| CaError::Tls(rustls::Error::General(e.to_string())))?;
        Ok((cert.der().clone(), key_der))
    }
}

fn ca_params() -> Result<CertificateParams> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Gabriel Local CA");
    dn.push(DnType::OrganizationName, "Gabriel (local development)");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];

    let now = gabriel_core::now_ms();
    let (y0, m0, d0) = gabriel_core::date_parts(now, -1);
    let (y1, m1, d1) = gabriel_core::date_parts(now, CA_VALIDITY_YEARS * 365);
    params.not_before = date_time_ymd(y0 as i32, m0 as u8, d0 as u8);
    params.not_after = date_time_ymd(y1 as i32, m1 as u8, d1 as u8);
    Ok(params)
}

fn first_certificate(pem: &str, path: &Path) -> Result<CertificateDer<'static>> {
    // Parsed with rustls-pki-types rather than rustls-pemfile: the latter was
    // marked unmaintained (RUSTSEC-2025-0134), and pki-types is already in the
    // tree because rustls depends on it, so this removes a dependency instead
    // of adding one.
    use rustls::pki_types::pem::PemObject as _;
    CertificateDer::from_pem_slice(pem.as_bytes()).map_err(|_| CaError::NoCertificate {
        path: path.to_path_buf(),
    })
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|source| CaError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write(path: &Path, bytes: &[u8], _mode: u32) -> Result<()> {
    let io = |source| CaError::Io {
        path: path.to_path_buf(),
        source,
    };

    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(_mode)
            .open(path)
            .map_err(io)?;
        file.write_all(bytes).map_err(io)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests run in parallel, so the directory has to be unique per call —
    /// a timestamp alone collides.
    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gabriel-ca-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn generates_a_ca_on_first_use_and_reuses_it_after() {
        let dir = temp_dir();
        let first = CertificateAuthority::load_or_create(&dir).unwrap();
        let pem = first.cert_pem().to_string();
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----"));

        let second = CertificateAuthority::load_or_create(&dir).unwrap();
        assert_eq!(
            second.cert_pem(),
            pem,
            "a second run must not re-mint the CA"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_private_key_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir();
        CertificateAuthority::load_or_create(&dir).unwrap();
        let mode = std::fs::metadata(dir.join(CA_KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "CA key readable by others: {mode:o}");
    }

    #[test]
    fn two_installs_get_different_authorities() {
        let a = CertificateAuthority::load_or_create(temp_dir()).unwrap();
        let b = CertificateAuthority::load_or_create(temp_dir()).unwrap();
        assert_ne!(
            a.cert_pem(),
            b.cert_pem(),
            "a shared MITM root across installs would be a backdoor"
        );
    }

    #[test]
    fn mints_and_caches_a_config_per_host() {
        let dir = temp_dir();
        let ca = CertificateAuthority::load_or_create(&dir).unwrap();

        let one = ca.server_config("api.test").unwrap();
        let again = ca.server_config("api.test").unwrap();
        assert!(Arc::ptr_eq(&one, &again), "host config should be cached");

        let other = ca.server_config("other.test").unwrap();
        assert!(!Arc::ptr_eq(&one, &other));
    }

    #[test]
    fn leaf_certificates_are_never_written_to_disk() {
        let dir = temp_dir();
        let ca = CertificateAuthority::load_or_create(&dir).unwrap();
        ca.server_config("api.test").unwrap();

        let files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 2, "unexpected files on disk: {files:?}");
    }
}
