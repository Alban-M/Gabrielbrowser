//! `gabriel doctor` — what a support conversation would otherwise ask.
//!
//! Every first-run problem this tool has is environmental rather than logical:
//! the CA is not trusted, the proxy port is taken, the vault cannot reach a
//! keychain, `HTTPS_PROXY` is quietly rerouting every request, the collection is
//! somewhere other than where the user thinks. None of those produce a good
//! error message at the point of failure, because the failure surfaces three
//! layers away from its cause.
//!
//! So the checks here answer the questions a maintainer would ask first, and
//! every failing one carries the fix rather than just the diagnosis. It runs
//! without a collection, without a network, and without creating anything.

use gabriel_collection::Collection;
use gabriel_vault::KeySource;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Working.
    Ok,
    /// Worth knowing, not a problem.
    Info,
    /// Will bite under some conditions.
    Warn,
    /// Broken now.
    Fail,
}

impl Status {
    pub fn marker(self) -> &'static str {
        match self {
            Status::Ok => "✓",
            Status::Info => "·",
            Status::Warn => "!",
            Status::Fail => "✗",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Info => "info",
            Status::Warn => "warning",
            Status::Fail => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub status: Status,
    /// What was found.
    pub detail: String,
    /// What to do about it. Only set when there is something to do.
    pub remedy: Option<String>,
}

impl Check {
    fn new(name: &str, status: Status, detail: impl Into<String>) -> Self {
        Check {
            name: name.to_string(),
            status,
            detail: detail.into(),
            remedy: None,
        }
    }

    fn with_remedy(mut self, remedy: impl Into<String>) -> Self {
        self.remedy = Some(remedy.into());
        self
    }
}

/// Everything doctor needs to know about where it is running. Passed in rather
/// than read from the world, so the checks are testable.
pub struct Environment {
    pub start_dir: PathBuf,
    /// Port the proxy would try to bind.
    pub proxy_port: u16,
    /// Read from the process environment; empty in tests.
    pub vars: Vec<(String, String)>,
}

impl Environment {
    pub fn detect(start_dir: PathBuf, proxy_port: u16) -> Self {
        // Only the variables that change behaviour, and never their values
        // unless the value is itself the point (a proxy URL).
        const INTERESTING: &[&str] = &[
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "GABRIEL_VAULT_PASSPHRASE",
            "NO_COLOR",
            "SSL_CERT_FILE",
        ];
        let vars = INTERESTING
            .iter()
            .filter_map(|name| {
                std::env::var(name)
                    .ok()
                    .or_else(|| std::env::var(name.to_lowercase()).ok())
                    .map(|value| (name.to_string(), value))
            })
            .collect();
        Environment {
            start_dir,
            proxy_port,
            vars,
        }
    }
}

/// Run every check. Order is deliberate: identity, then storage, then network.
pub fn check_all(env: &Environment) -> Vec<Check> {
    let mut checks = vec![
        Check::new("version", Status::Ok, env!("CARGO_PKG_VERSION")),
        Check::new(
            "platform",
            Status::Ok,
            format!(
                "{} {} ({})",
                std::env::consts::OS,
                std::env::consts::ARCH,
                std::env::consts::FAMILY
            ),
        ),
    ];

    checks.push(check_permissions_model());
    checks.push(check_temp_dir());

    let collection = Collection::discover(&env.start_dir).ok();
    checks.push(check_collection(collection.as_ref(), &env.start_dir));

    if let Some(collection) = &collection {
        checks.push(check_runtime_dir(collection));
        checks.push(check_vault(collection));
        checks.push(check_ca(collection));
        checks.extend(check_runtime_permissions(collection));
    }

    checks.push(check_proxy_port(env.proxy_port));
    checks.extend(check_environment_vars(env));
    checks
}

fn check_permissions_model() -> Check {
    match gabriel_core::permission_warning() {
        None => Check::new(
            "file permissions",
            Status::Ok,
            "enforced (vault, sessions, captures and CA key are 0600)",
        ),
        Some(warning) => Check::new("file permissions", Status::Warn, warning)
            .with_remedy("keep the collection in a directory only your account can read"),
    }
}

fn check_temp_dir() -> Check {
    let dir = std::env::temp_dir();
    let probe = dir.join(format!("gabriel-doctor-{}", std::process::id()));
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Check::new("temp directory", Status::Ok, dir.display().to_string())
        }
        Err(e) => Check::new(
            "temp directory",
            Status::Fail,
            format!("{} is not writable: {e}", dir.display()),
        )
        .with_remedy("set TMPDIR to a directory you can write to"),
    }
}

fn check_collection(collection: Option<&Collection>, start_dir: &Path) -> Check {
    match collection {
        Some(collection) => {
            let requests = collection.requests().len();
            let problems = collection.problems().len();
            let detail = format!(
                "{} ({requests} request{}{})",
                collection.root().display(),
                if requests == 1 { "" } else { "s" },
                if problems > 0 {
                    format!(", {problems} unreadable")
                } else {
                    String::new()
                }
            );
            if problems > 0 {
                Check::new("collection", Status::Warn, detail)
                    .with_remedy("run `gabriel ls` to see which files could not be read")
            } else {
                Check::new("collection", Status::Ok, detail)
            }
        }
        None => Check::new(
            "collection",
            Status::Info,
            format!("none found in {} or any parent", start_dir.display()),
        )
        .with_remedy("run `gabriel init` to create one"),
    }
}

fn check_runtime_dir(collection: &Collection) -> Check {
    let dir = collection.runtime_dir();
    if !dir.exists() {
        return Check::new(
            "runtime directory",
            Status::Warn,
            format!("{} does not exist yet", dir.display()),
        )
        .with_remedy("it is created on first use; `gabriel init` makes it up front");
    }
    let probe = dir.join(".doctor-probe");
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Check::new("runtime directory", Status::Ok, dir.display().to_string())
        }
        Err(e) => Check::new(
            "runtime directory",
            Status::Fail,
            format!("{} is not writable: {e}", dir.display()),
        )
        .with_remedy("check ownership of the collection directory"),
    }
}

fn check_vault(collection: &Collection) -> Check {
    let path = collection.vault_path();
    let source = KeySource::from_environment();
    let source_name = match source {
        KeySource::Keychain => "OS keychain",
        KeySource::Passphrase(_) => "passphrase (GABRIEL_VAULT_PASSPHRASE)",
    };

    if !path.exists() {
        // Not an error: a collection with no secrets never needs one.
        return Check::new(
            "vault",
            Status::Info,
            format!("not created yet; will use the {source_name}"),
        );
    }

    // Opening is the only honest test of "available", and it is read-only.
    match gabriel_vault::Vault::open(&path, &source) {
        Ok(vault) => Check::new(
            "vault",
            Status::Ok,
            format!(
                "{} secret{} via the {source_name}",
                vault.len(),
                if vault.len() == 1 { "" } else { "s" }
            ),
        ),
        Err(e) => Check::new("vault", Status::Fail, format!("cannot be opened: {e}")).with_remedy(
            match source {
                KeySource::Keychain => {
                    "the keychain entry may be missing; set GABRIEL_VAULT_PASSPHRASE to use a \
                     passphrase instead, or delete the vault and re-add its secrets"
                }
                KeySource::Passphrase(_) => {
                    "GABRIEL_VAULT_PASSPHRASE does not match the one the vault was created with"
                }
            },
        ),
    }
}

fn check_ca(collection: &Collection) -> Check {
    let dir = collection.runtime_dir();
    let cert = dir.join(gabriel_proxy::ca::CA_CERT_FILE);
    let key = dir.join(gabriel_proxy::ca::CA_KEY_FILE);

    if !cert.exists() {
        return Check::new(
            "interception CA",
            Status::Info,
            "not generated yet; created on the first `gabriel capture start`",
        );
    }
    if !key.exists() {
        return Check::new(
            "interception CA",
            Status::Fail,
            format!("{} exists but its key is missing", cert.display()),
        )
        .with_remedy("delete the certificate; a new pair is generated on next use");
    }

    // Parsing it is the difference between "a file is there" and "it works".
    match std::fs::read_to_string(&cert) {
        Ok(pem) if pem.contains("BEGIN CERTIFICATE") => Check::new(
            "interception CA",
            Status::Ok,
            format!("{} (trust it once with `gabriel ca`)", cert.display()),
        ),
        Ok(_) => Check::new(
            "interception CA",
            Status::Fail,
            format!("{} is not a PEM certificate", cert.display()),
        )
        .with_remedy("delete both CA files; a new pair is generated on next use"),
        Err(e) => Check::new(
            "interception CA",
            Status::Fail,
            format!("{} cannot be read: {e}", cert.display()),
        ),
    }
}

/// On Unix, confirm the files that hold credentials are actually `0600`.
fn check_runtime_permissions(collection: &Collection) -> Vec<Check> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = collection.runtime_dir();
        let sensitive = [
            ("vault", collection.vault_path()),
            ("sessions", collection.sessions_path()),
            ("captures", collection.captures_path()),
            ("CA key", dir.join(gabriel_proxy::ca::CA_KEY_FILE)),
        ];

        let mut exposed = Vec::new();
        for (label, path) in &sensitive {
            let Ok(meta) = std::fs::metadata(path) else {
                continue;
            };
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                exposed.push(format!("{label} ({:o})", mode & 0o777));
            }
        }

        vec![if exposed.is_empty() {
            Check::new(
                "credential file modes",
                Status::Ok,
                "everything sensitive is 0600",
            )
        } else {
            Check::new(
                "credential file modes",
                Status::Fail,
                format!("readable by others: {}", exposed.join(", ")),
            )
            .with_remedy("run `chmod 600` on them; anything writing them fresh will be correct")
        }]
    }
    #[cfg(not(unix))]
    {
        let _ = collection;
        Vec::new()
    }
}

fn check_proxy_port(port: u16) -> Check {
    // Binding and dropping is the only way to know, and costs nothing.
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            drop(listener);
            Check::new(
                "proxy port",
                Status::Ok,
                format!("127.0.0.1:{port} is free"),
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Check::new(
            "proxy port",
            Status::Warn,
            format!("127.0.0.1:{port} is already in use"),
        )
        .with_remedy("another Gabriel may be running; use `gabriel capture start --port <other>`"),
        Err(e) => Check::new(
            "proxy port",
            Status::Fail,
            format!("127.0.0.1:{port} cannot be bound: {e}"),
        ),
    }
}

fn check_environment_vars(env: &Environment) -> Vec<Check> {
    let mut checks = Vec::new();

    // An outer proxy silently reroutes every request the engine makes, which is
    // the single most confusing thing that can be true of this environment.
    let outer_proxy: Vec<&(String, String)> = env
        .vars
        .iter()
        .filter(|(name, _)| matches!(name.as_str(), "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY"))
        .collect();
    if outer_proxy.is_empty() {
        checks.push(Check::new(
            "outbound proxy",
            Status::Ok,
            "no HTTP_PROXY/HTTPS_PROXY in the environment",
        ));
    } else {
        // A proxy URL routinely carries `user:password@`, and doctor output is
        // something people paste into bug reports — that is what it is for. The
        // host is the diagnostic part; the credentials are not.
        let detail = outer_proxy
            .iter()
            .map(|(name, value)| format!("{name}={}", crate::feedback::scrub(value)))
            .collect::<Vec<_>>()
            .join(", ");
        checks.push(
            Check::new("outbound proxy", Status::Warn, detail).with_remedy(
                "requests will be routed through it; unset it if that is not intended",
            ),
        );
    }

    if env
        .vars
        .iter()
        .any(|(name, _)| name == "GABRIEL_VAULT_PASSPHRASE")
    {
        checks.push(Check::new(
            "vault passphrase",
            Status::Info,
            "GABRIEL_VAULT_PASSPHRASE is set; the keychain will not be used",
        ));
    }

    if let Some((_, path)) = env.vars.iter().find(|(name, _)| name == "SSL_CERT_FILE") {
        checks.push(
            Check::new("SSL_CERT_FILE", Status::Info, path.clone())
                .with_remedy("TLS verification will use this bundle instead of the system store"),
        );
    }

    checks
}

/// Worst status across the checks, which decides the exit code.
pub fn worst(checks: &[Check]) -> Status {
    if checks.iter().any(|c| c.status == Status::Fail) {
        Status::Fail
    } else if checks.iter().any(|c| c.status == Status::Warn) {
        Status::Warn
    } else {
        Status::Ok
    }
}

pub fn to_json(checks: &[Check]) -> String {
    let entries: Vec<serde_json::Value> = checks
        .iter()
        .map(|check| {
            serde_json::json!({
                "name": check.name,
                "status": check.status.as_str(),
                "detail": check.detail,
                "remedy": check.remedy,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "status": worst(checks).as_str(),
        "checks": entries,
    }))
    .expect("checks serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_credentials_are_not_printed_back() {
        // doctor output is meant to be pasted into a bug report, so a password
        // in HTTPS_PROXY must not survive the trip.
        let env = Environment {
            start_dir: std::env::temp_dir(),
            proxy_port: 0,
            vars: vec![(
                "HTTPS_PROXY".to_string(),
                "http://alice:hunter2@proxy.corp:8080".to_string(),
            )],
        };
        let checks = check_all(&env);
        let proxy = checks
            .iter()
            .find(|c| c.name == "outbound proxy")
            .expect("no outbound proxy check");

        assert!(!proxy.detail.contains("hunter2"), "{}", proxy.detail);
        // Still says enough to be worth reading.
        assert!(proxy.detail.contains("proxy.corp:8080"), "{}", proxy.detail);
    }

    fn temp_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gabriel-doctor-test-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn env_for(dir: PathBuf) -> Environment {
        Environment {
            start_dir: dir,
            // Port 0 always binds, so the port check is not a source of
            // flakiness in tests that are not about it.
            proxy_port: 0,
            vars: Vec::new(),
        }
    }

    fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no check named {name}; have {:?}", names(checks)))
    }

    fn names(checks: &[Check]) -> Vec<&str> {
        checks.iter().map(|c| c.name.as_str()).collect()
    }

    /// Doctor has to work where nothing is set up — that is exactly when it is
    /// reached for.
    #[test]
    fn it_runs_outside_a_collection_without_failing() {
        let checks = check_all(&env_for(temp_dir("bare")));

        assert_eq!(find(&checks, "collection").status, Status::Info);
        assert!(find(&checks, "collection").remedy.is_some());
        assert_eq!(find(&checks, "version").detail, env!("CARGO_PKG_VERSION"));
        assert_ne!(
            worst(&checks),
            Status::Fail,
            "a bare directory is not a failure"
        );
    }

    #[test]
    fn it_reports_a_healthy_collection() {
        let dir = temp_dir("healthy");
        Collection::init(&dir, "demo").expect("init");

        let checks = check_all(&env_for(dir));
        assert_eq!(find(&checks, "collection").status, Status::Ok);
        assert!(find(&checks, "collection").detail.contains("1 request"));
        assert_eq!(find(&checks, "runtime directory").status, Status::Ok);
        // A fresh collection has neither, and that is fine rather than broken.
        assert_eq!(find(&checks, "vault").status, Status::Info);
        assert_eq!(find(&checks, "interception CA").status, Status::Info);
        assert_ne!(worst(&checks), Status::Fail);
    }

    #[test]
    fn an_unreadable_request_file_is_surfaced() {
        let dir = temp_dir("broken");
        let collection = Collection::init(&dir, "demo").expect("init");
        std::fs::write(
            collection.root().join("requests").join("bad.toml"),
            "not { toml",
        )
        .expect("write");

        let checks = check_all(&env_for(dir));
        let check = find(&checks, "collection");
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("unreadable"), "{}", check.detail);
    }

    #[test]
    fn a_corrupt_ca_certificate_is_a_failure_with_a_fix() {
        let dir = temp_dir("ca");
        let collection = Collection::init(&dir, "demo").expect("init");
        let runtime = collection.runtime_dir();
        std::fs::write(runtime.join(gabriel_proxy::ca::CA_CERT_FILE), "garbage").expect("write");
        std::fs::write(runtime.join(gabriel_proxy::ca::CA_KEY_FILE), "garbage").expect("write");

        let check = check_ca(&Collection::load(collection.root()).expect("load"));
        assert_eq!(check.status, Status::Fail);
        assert!(check.remedy.is_some(), "a failure must say what to do");
    }

    #[test]
    fn a_certificate_without_its_key_is_a_failure() {
        let dir = temp_dir("ca-halfkey");
        let collection = Collection::init(&dir, "demo").expect("init");
        std::fs::write(
            collection
                .runtime_dir()
                .join(gabriel_proxy::ca::CA_CERT_FILE),
            "-----BEGIN CERTIFICATE-----\nx\n-----END CERTIFICATE-----",
        )
        .expect("write");

        let check = check_ca(&collection);
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("key is missing"), "{}", check.detail);
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_credential_file_is_reported() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = temp_dir("modes");
        let collection = Collection::init(&dir, "demo").expect("init");
        let sessions = collection.sessions_path();
        std::fs::write(&sessions, "{}").expect("write");
        std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        let checks = check_runtime_permissions(&collection);
        let check = &checks[0];
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("sessions"), "{}", check.detail);
        assert!(check.detail.contains("644"), "{}", check.detail);
    }

    #[test]
    fn a_port_in_use_is_a_warning_and_names_the_way_out() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();

        let check = check_proxy_port(port);
        assert_eq!(check.status, Status::Warn);
        assert!(
            check
                .remedy
                .as_deref()
                .is_some_and(|r| r.contains("--port"))
        );
    }

    #[test]
    fn an_outer_proxy_is_flagged_because_it_reroutes_everything() {
        let mut env = env_for(temp_dir("proxyvar"));
        env.vars = vec![("HTTPS_PROXY".into(), "http://corp-proxy:3128".into())];

        let checks = check_environment_vars(&env);
        let check = find(&checks, "outbound proxy");
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("corp-proxy"), "{}", check.detail);
    }

    #[test]
    fn a_clean_environment_says_so() {
        let checks = check_environment_vars(&env_for(temp_dir("cleanenv")));
        assert_eq!(find(&checks, "outbound proxy").status, Status::Ok);
    }

    #[test]
    fn the_worst_status_decides_the_verdict() {
        let ok = Check::new("a", Status::Ok, "");
        let info = Check::new("b", Status::Info, "");
        let warn = Check::new("c", Status::Warn, "");
        let fail = Check::new("d", Status::Fail, "");

        assert_eq!(worst(&[ok.clone(), info.clone()]), Status::Ok);
        assert_eq!(worst(&[ok.clone(), warn.clone()]), Status::Warn);
        assert_eq!(worst(&[warn.clone(), fail.clone()]), Status::Fail);
        assert_eq!(worst(&[]), Status::Ok);
    }

    #[test]
    fn the_json_form_is_machine_readable() {
        let checks = check_all(&env_for(temp_dir("json")));
        let parsed: serde_json::Value =
            serde_json::from_str(&to_json(&checks)).expect("valid JSON");

        assert!(parsed["status"].is_string());
        let entries = parsed["checks"].as_array().expect("array");
        assert_eq!(entries.len(), checks.len());
        assert!(
            entries
                .iter()
                .all(|e| e["name"].is_string() && e["status"].is_string())
        );
    }

    /// Doctor is what someone runs when things are already wrong; it must not
    /// create files, or the first run would change what the second one sees.
    #[test]
    fn it_creates_nothing() {
        let dir = temp_dir("readonly");
        let collection = Collection::init(&dir, "demo").expect("init");
        let before = listing(collection.root());

        check_all(&env_for(dir.clone()));

        assert_eq!(before, listing(collection.root()), "doctor wrote something");
    }

    fn listing(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for entry in walk(root) {
            out.push(entry);
        }
        out.sort();
        out
    }

    fn walk(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            out.push(path.display().to_string());
            if path.is_dir() {
                out.extend(walk(&path));
            }
        }
        out
    }
}
