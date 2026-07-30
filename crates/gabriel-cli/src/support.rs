//! Small pieces of CLI logic worth testing on their own.

use crate::output::{self, Style};
use anyhow::{Context, Result};
use gabriel_collection::{Collection, Environment};
use gabriel_core::ExecutedResponse;
use gabriel_core::capture::Capture;
use gabriel_core::response::Timings;
use gabriel_core::vars::SecretProvider;
use gabriel_vault::{KeySource, Vault};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// A vault that is only opened if a `{{secret:…}}` is actually referenced.
///
/// This matters more than it looks: opening the vault touches the OS keychain,
/// and on macOS that can raise an authorisation prompt. A request with no
/// secrets in it should never make the machine ask.
pub struct LazySecrets {
    path: PathBuf,
    source: KeySource,
    opened: RefCell<Option<Option<Vault>>>,
}

impl LazySecrets {
    pub fn new(path: impl Into<PathBuf>, source: KeySource) -> Self {
        LazySecrets { path: path.into(), source, opened: RefCell::new(None) }
    }
}

impl SecretProvider for LazySecrets {
    fn secret(&self, key: &str) -> Option<String> {
        let mut opened = self.opened.borrow_mut();
        let vault = opened.get_or_insert_with(|| {
            if !self.path.exists() {
                return None;
            }
            match Vault::open(&self.path, &self.source) {
                Ok(vault) => Some(vault),
                Err(error) => {
                    eprintln!("warning: could not open the vault: {error}");
                    None
                }
            }
        });
        vault.as_ref().and_then(|vault| vault.secret(key))
    }
}

/// Split `key=value`, keeping any `=` in the value.
pub fn parse_assignment(input: &str) -> Result<(String, String)> {
    let (key, value) = input
        .split_once('=')
        .with_context(|| format!("expected KEY=VALUE, got `{input}`"))?;
    let key = key.trim();
    if key.is_empty() {
        anyhow::bail!("expected KEY=VALUE, got `{input}`");
    }
    Ok((key.to_string(), value.to_string()))
}

/// The name to give a collection created in `dir`.
pub fn directory_name(dir: &Path) -> String {
    dir.canonicalize()
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "collection".to_string())
}

/// Capture ids are shown in full. Truncating them looks tidier but produces
/// prefixes shared by captures recorded in the same millisecond, and
/// `store.get` resolves by prefix — a shortened id would silently select the
/// wrong capture.
pub fn display_id(id: &str) -> &str {
    id
}

/// Adapt a stored capture into the shape the diff and viewer expect.
pub fn capture_to_response(capture: &Capture) -> ExecutedResponse {
    let response = capture.response.as_ref();
    ExecutedResponse {
        status: response.map(|r| r.status).unwrap_or(0),
        status_text: response.map(|r| r.status_text.clone()).unwrap_or_default(),
        http_version: capture.request.http_version.clone(),
        headers: response.map(|r| r.headers.clone()).unwrap_or_default(),
        body: response
            .and_then(|r| r.body.as_ref())
            .map(|b| b.bytes())
            .unwrap_or_default(),
        timings: Timings { ttfb_ms: 0, total_ms: capture.duration_ms },
        final_url: capture.request.url.clone(),
    }
}

/// Add a variable to an environment file unless it is already set. Returns
/// whether anything was written — an existing value is never overwritten,
/// because the developer's own value outranks one inferred from traffic.
pub fn set_environment_var(
    collection: &Collection,
    env_name: &str,
    key: &str,
    value: &str,
) -> Result<bool> {
    let path = collection
        .root()
        .join(gabriel_collection::ENVIRONMENTS_DIR)
        .join(format!("{env_name}.toml"));

    let mut environment = if path.exists() {
        collection.environment(env_name)?
    } else {
        Environment { name: Some(env_name.to_string()), ..Default::default() }
    };

    if environment.vars.contains_key(key) || environment.secrets.contains_key(key) {
        return Ok(false);
    }
    environment.vars.insert(key.to_string(), value.to_string());
    gabriel_collection::write_toml(&path, &environment)?;
    Ok(true)
}

pub fn print_capture(capture: &Capture, style: &Style) {
    println!(
        "{} {}",
        style.bold(&capture.request.method),
        capture.request.url
    );
    println!(
        "{} {}",
        style.dim("at"),
        gabriel_core::format_iso8601(capture.at)
    );
    if let Some(page) = &capture.page {
        println!("{} {page}", style.dim("from"));
    }
    if let Some(session) = &capture.session {
        println!("{} {session}", style.dim("session"));
    }

    println!();
    println!("{}", style.bold("request"));
    for (name, value) in capture.request.headers.iter_pairs() {
        println!("  {} {}", style.dim(&format!("{name}:")), redact_credential(name, value));
    }
    if let Some(body) = &capture.request.body {
        println!();
        println!("{}", body_preview(body));
    }

    if let Some(response) = &capture.response {
        println!();
        println!(
            "{} {} {}",
            style.bold("response"),
            style.status(response.status),
            style.dim(&response.status_text)
        );
        for (name, value) in response.headers.iter_pairs() {
            println!("  {} {}", style.dim(&format!("{name}:")), value);
        }
        if let Some(body) = &response.body {
            println!();
            println!("{}", body_preview(body));
        }
    }
}

/// Credentials seen in captured traffic are shown masked. `gabriel promote` moves
/// them to the vault; nothing is served by printing them at a terminal.
fn redact_credential(name: &str, value: &str) -> String {
    const SENSITIVE: &[&str] = &["authorization", "cookie", "proxy-authorization", "x-api-key"];
    if SENSITIVE.contains(&name.to_ascii_lowercase().as_str()) {
        let head: String = value.chars().take(12).collect();
        format!("{head}… ••••")
    } else {
        value.to_string()
    }
}

fn body_preview(body: &gabriel_core::capture::CapturedBody) -> String {
    match body.as_text() {
        Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(json) => output::truncate(
                &serde_json::to_string_pretty(&json).unwrap_or_default(),
                4000,
            ),
            Err(_) => output::truncate(text, 4000),
        },
        None => format!("<{} of binary data>", gabriel_core::format_bytes(body.len())),
    }
}

pub fn print_ca_instructions(cert_path: &Path, style: &Style) {
    println!("{} {}", style.bold("CA certificate:"), cert_path.display());
    println!();
    println!(
        "{}",
        style.dim("This CA was generated for this machine alone and never leaves it.")
    );
    println!(
        "{}",
        style.dim("Trusting it lets Gabriel read HTTPS traffic you send through the proxy.")
    );
    println!();

    if cfg!(target_os = "macos") {
        println!("{}", style.bold("macOS (system trust, needs your password):"));
        println!(
            "  sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
            cert_path.display()
        );
    } else if cfg!(target_os = "linux") {
        println!("{}", style.bold("Linux (Debian/Ubuntu):"));
        println!("  sudo cp {} /usr/local/share/ca-certificates/gabriel-ca.crt", cert_path.display());
        println!("  sudo update-ca-certificates");
    } else if cfg!(target_os = "windows") {
        println!("{}", style.bold("Windows (elevated PowerShell):"));
        println!(
            "  Import-Certificate -FilePath '{}' -CertStoreLocation Cert:\\LocalMachine\\Root",
            cert_path.display()
        );
    }

    println!();
    println!("{}", style.bold("Firefox keeps its own store:"));
    println!("  Settings → Privacy & Security → Certificates → View Certificates → Import");
    println!();
    println!(
        "{}",
        style.dim("Remove it when you're done; a trusted MITM root is a standing risk.")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use gabriel_core::capture::{CapturedBody, CapturedRequest, CapturedResponse};
    use gabriel_core::model::FieldMap;

    #[test]
    fn assignments_split_on_the_first_equals() {
        assert_eq!(
            parse_assignment("token=abc=def").unwrap(),
            ("token".to_string(), "abc=def".to_string())
        );
    }

    #[test]
    fn assignments_allow_an_empty_value() {
        assert_eq!(
            parse_assignment("token=").unwrap(),
            ("token".to_string(), String::new())
        );
    }

    #[test]
    fn malformed_assignments_are_rejected() {
        assert!(parse_assignment("token").is_err());
        assert!(parse_assignment("=value").is_err());
    }

    #[test]
    fn credentials_are_masked_for_display() {
        let masked = redact_credential("Authorization", "Bearer sk-live-supersecretvalue");
        assert!(!masked.contains("supersecret"), "{masked}");
        let plain = redact_credential("Accept", "application/json");
        assert_eq!(plain, "application/json");
    }

    fn capture(status: u16, body: &str) -> Capture {
        Capture {
            id: "cap_0123456789abcdef".into(),
            at: 1_750_000_000_000,
            duration_ms: 12,
            session: None,
            page: None,
            request: CapturedRequest {
                method: "GET".into(),
                url: "https://api.test/users".into(),
                http_version: "HTTP/2".into(),
                headers: FieldMap::default(),
                body: None,
            },
            response: Some(CapturedResponse {
                status,
                status_text: "OK".into(),
                headers: FieldMap::default(),
                body: Some(CapturedBody::Text { text: body.to_string() }),
            }),
        }
    }

    #[test]
    fn captures_convert_into_comparable_responses() {
        let response = capture_to_response(&capture(200, r#"{"a":1}"#));
        assert_eq!(response.status, 200);
        assert_eq!(response.text(), r#"{"a":1}"#);
        assert_eq!(response.timings.total_ms, 12);
    }

    #[test]
    fn a_capture_without_a_response_converts_to_status_zero() {
        let mut capture = capture(200, "{}");
        capture.response = None;
        let response = capture_to_response(&capture);
        assert_eq!(response.status, 0);
        assert!(response.body.is_empty());
    }

    #[test]
    fn displayed_ids_are_usable_as_lookup_keys() {
        // Two captures recorded in the same millisecond differ only in their
        // tail, so anything shorter than the whole id is ambiguous.
        let first = "c19faf40360000";
        let second = "c19faf40360001";
        assert_ne!(display_id(first), display_id(second));
        assert!(second.starts_with(display_id(second)));
    }

    #[test]
    fn a_missing_vault_yields_no_secrets_and_no_prompt() {
        let missing = std::env::temp_dir().join("gabriel-nonexistent-vault-xyz/vault.json");
        let secrets = LazySecrets::new(missing, KeySource::Passphrase("unused-passphrase".into()));
        assert_eq!(secrets.secret("anything"), None);
    }

    fn temp_collection() -> Collection {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gabriel-cli-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Collection::init(&dir, "test").unwrap()
    }

    #[test]
    fn environment_variables_can_be_added() {
        let collection = temp_collection();
        assert!(set_environment_var(&collection, "dev", "api_url", "https://api.test").unwrap());

        let env = collection.environment("dev").unwrap();
        assert_eq!(env.vars.get("api_url").unwrap(), "https://api.test");
    }

    #[test]
    fn an_existing_variable_is_never_overwritten() {
        let collection = temp_collection();
        // `dev` ships with base_url already set.
        assert!(!set_environment_var(&collection, "dev", "base_url", "https://other.test").unwrap());

        let env = collection.environment("dev").unwrap();
        assert_eq!(env.vars.get("base_url").unwrap(), "https://httpbin.org");
    }

    #[test]
    fn a_new_environment_file_is_created_on_demand() {
        let collection = temp_collection();
        assert!(set_environment_var(&collection, "prod", "base_url", "https://api.test").unwrap());
        assert!(collection.environment_names().contains(&"prod".to_string()));
    }
}
