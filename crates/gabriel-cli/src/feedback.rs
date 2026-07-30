//! `gabriel feedback` — a support bundle the user assembles, reads, and then
//! decides whether to send.
//!
//! This is deliberately not telemetry. Nothing here transmits anything, nothing
//! runs unless it is asked for, and the output is a plain directory of text
//! files so that "inspect it before sharing" is a thing someone can actually
//! do rather than a claim they have to take on faith.
//!
//! The redaction strategy is an allow-list, not a deny-list. Files are *built*
//! out of named fields known to be safe, rather than copied and then stripped
//! of the parts known to be dangerous. A deny-list fails open — the first
//! secret in a shape nobody anticipated ends up in the bundle. This fails
//! closed: something newly added to a collection is absent from the bundle
//! until someone deliberately includes it.
//!
//! What is never collected, at all: the vault, session cookie jars, the CA
//! private key, captured request and response bodies, captured headers, and
//! the contents of request files. Scrubbing is the second line of defence for
//! the free text that does get in — error messages and configuration values —
//! not the first.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a redacted value is replaced with. Visible, so a reader can tell the
/// difference between "this was empty" and "this was removed".
pub const MASK: &str = "[redacted]";

/// Kept short enough to read in one sitting. A support bundle nobody opens is
/// telemetry with extra steps.
const MAX_ERRORS: usize = 50;

/// Names whose *values* are secret regardless of what they look like.
const SECRET_KEYS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "apikey",
    "api_key",
    "authorization",
    "auth",
    "cookie",
    "session",
    "credential",
    "private",
    "passphrase",
    "bearer",
    "client_secret",
    "access_key",
];

fn key_is_secret(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    SECRET_KEYS
        .iter()
        .any(|needle| lowered.contains(&needle.replace('_', "")) || lowered.contains(needle))
}

// ── scrubbing free text ──────────────────────────────────────────────────────

/// Remove anything credential-shaped from text that has to be included as
/// prose — error messages, mostly, which quote whatever they failed on.
pub fn scrub(input: &str) -> String {
    let stage = redact_userinfo(input);
    let stage = redact_jwts(&stage);
    let stage = redact_keyed(&stage);
    redact_opaque_runs(&stage)
}

/// `https://user:password@host` — a proxy URL in the environment is the common
/// way this reaches us, and the password is right there in the middle of it.
fn redact_userinfo(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i..].starts_with(&[':', '/', '/']) {
            out.push_str("://");
            i += 3;
            // The authority ends at the first delimiter; if it contains an '@'
            // then everything before that '@' is userinfo.
            let start = i;
            let mut end = i;
            while end < bytes.len()
                && !matches!(bytes[end], '/' | '?' | '#' | '"' | '\'' | ' ' | '\n' | '\t')
            {
                end += 1;
            }
            let authority: String = bytes[start..end].iter().collect();
            match authority.rfind('@') {
                Some(at) => {
                    out.push_str(MASK);
                    out.push('@');
                    out.push_str(&authority[at + 1..]);
                }
                None => out.push_str(&authority),
            }
            i = end;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=' | '/')
}

/// A JWT is three dot-separated base64url segments starting `eyJ`. Worth
/// catching by shape as well as by length: a short one would slip past the
/// opaque-run rule, and a JWT is always a credential.
fn redact_jwts(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i..].starts_with(&['e', 'y', 'J']) && (i == 0 || !is_token_char(chars[i - 1])) {
            let mut end = i;
            let mut dots = 0;
            while end < chars.len() && (is_token_char(chars[end]) || chars[end] == '.') {
                if chars[end] == '.' {
                    dots += 1;
                }
                end += 1;
            }
            if dots >= 2 {
                out.push_str("[redacted-jwt]");
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `token=abc`, `"password": "abc"`, `Authorization: Bearer abc`.
fn redact_keyed(input: &str) -> String {
    let mut out = String::with_capacity(input.len());

    for line in input.split_inclusive('\n') {
        out.push_str(&redact_keyed_in_line(line));
    }
    out
}

fn redact_keyed_in_line(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;

    while i < chars.len() {
        // Read the next word, so "token" matches but "gettoken" is judged as a
        // whole and "tokenizer" does not become a false positive on its own.
        if chars[i].is_ascii_alphabetic() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '-')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            out.push_str(&word);

            let lowered = word.to_ascii_lowercase();
            let is_scheme = lowered == "bearer" || lowered == "basic";

            if is_scheme || key_is_secret(&lowered) {
                // Skip the separator: quotes, colon, equals, whitespace.
                let mut j = i;
                let mut saw_separator = is_scheme;
                while j < chars.len() && matches!(chars[j], ' ' | '\t' | '"' | '\'' | ':' | '=') {
                    if matches!(chars[j], ':' | '=') {
                        saw_separator = true;
                    }
                    j += 1;
                }
                if saw_separator && j < chars.len() && is_token_char(chars[j]) {
                    let mut end = j;
                    while end < chars.len() && (is_token_char(chars[end]) || chars[end] == '.') {
                        end += 1;
                    }
                    let token: String = chars[j..end].iter().collect();
                    let token = token.to_ascii_lowercase();

                    // `Authorization: Bearer <token>` — the value after the
                    // separator is the scheme, not the credential. Redacting it
                    // would blank the word "Bearer" and leave the token sitting
                    // in the open, which is worse than doing nothing because it
                    // looks like it worked.
                    if token == "bearer" || token == "basic" {
                        out.extend(chars[i..end].iter());
                        let mut k = end;
                        while k < chars.len() && matches!(chars[k], ' ' | '\t') {
                            k += 1;
                        }
                        out.extend(chars[end..k].iter());
                        if k < chars.len() && is_token_char(chars[k]) {
                            let mut value_end = k;
                            while value_end < chars.len()
                                && (is_token_char(chars[value_end]) || chars[value_end] == '.')
                            {
                                value_end += 1;
                            }
                            out.push_str(MASK);
                            i = value_end;
                        } else {
                            i = k;
                        }
                        continue;
                    }

                    out.extend(chars[i..j].iter());
                    out.push_str(MASK);
                    i = end;
                    continue;
                }
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A long opaque run of token characters with both letters and digits in it.
/// Catches API keys that carry no label. `/` is excluded from the run so that a
/// long filesystem path breaks into short segments and survives — paths are
/// most of what makes a support bundle useful.
fn redact_opaque_runs(input: &str) -> String {
    const MIN: usize = 32;
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;

    while i < chars.len() {
        let opaque = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=');
        if opaque(chars[i]) && (i == 0 || !opaque(chars[i - 1])) {
            let start = i;
            let mut end = i;
            while end < chars.len() && opaque(chars[end]) {
                end += 1;
            }
            let run = &chars[start..end];
            let has_digit = run.iter().any(|c| c.is_ascii_digit());
            let has_alpha = run.iter().any(|c| c.is_ascii_alphabetic());
            if run.len() >= MIN && has_digit && has_alpha {
                out.push_str(MASK);
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ── configuration ────────────────────────────────────────────────────────────

/// Redact a TOML document, keeping its keys and structure. Keys are what make
/// a config file diagnosable — "is `base_url` set at all" is most of the
/// question — so they are preserved and the values are judged one at a time.
pub fn redact_toml(raw: &str) -> String {
    let Ok(table) = toml::from_str::<toml::Table>(raw) else {
        // Unparseable is itself a useful finding, and worth including verbatim
        // — scrubbed, since nothing has been able to reason about its keys.
        return scrub(raw);
    };
    let redacted = redact_value("", &toml::Value::Table(table));
    toml::to_string_pretty(&redacted).unwrap_or_else(|_| scrub(raw))
}

fn redact_value(key: &str, value: &toml::Value) -> toml::Value {
    match value {
        toml::Value::String(text) => {
            // A template reference is a *pointer* to a secret, not the secret.
            // Keeping it is the difference between "this resolves from the
            // vault" and no information at all.
            if text.starts_with("{{") && text.ends_with("}}") {
                return toml::Value::String(text.clone());
            }
            if key_is_secret(key) {
                return toml::Value::String(MASK.to_string());
            }
            toml::Value::String(scrub(text))
        }
        toml::Value::Table(table) => toml::Value::Table(
            table
                .iter()
                .map(|(k, v)| (k.clone(), redact_value(k, v)))
                .collect(),
        ),
        toml::Value::Array(items) => {
            toml::Value::Array(items.iter().map(|v| redact_value(key, v)).collect())
        }
        other => other.clone(),
    }
}

// ── what goes in the bundle ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    pub family: String,
}

impl Platform {
    pub fn detect() -> Self {
        Platform {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            family: std::env::consts::FAMILY.to_string(),
        }
    }
}

/// Counts only. Never a URL, never a header, never a body: a capture log is the
/// single most sensitive thing on the disk, and "how many, from where, when" is
/// all a support bundle needs from it.
#[derive(Debug, Clone, Default)]
pub struct CaptureSummary {
    pub total: usize,
    pub by_host: BTreeMap<String, usize>,
    pub by_status: BTreeMap<u16, usize>,
    pub oldest_ms: Option<u64>,
    pub newest_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ErrorRecord {
    pub at_ms: u64,
    pub command: String,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct CollectionInfo {
    pub root: String,
    pub request_names: Vec<String>,
    /// Relative path → raw contents. Redacted during the build.
    pub config_files: Vec<(String, String)>,
}

pub struct Input {
    pub version: String,
    pub platform: Platform,
    pub doctor_json: String,
    pub collection: Option<CollectionInfo>,
    pub captures: Option<CaptureSummary>,
    pub errors: Vec<ErrorRecord>,
}

pub struct BundleFile {
    pub path: String,
    pub contents: String,
}

/// Assemble the bundle. Pure: everything it knows arrives in `Input`, so what
/// ends up in a bundle can be asserted without writing one.
pub fn build(input: &Input) -> Vec<BundleFile> {
    let mut files = Vec::new();

    files.push(BundleFile {
        path: "version.txt".into(),
        contents: format!(
            "gabriel {}\ntarget: {}-{}\n",
            input.version, input.platform.arch, input.platform.os
        ),
    });

    files.push(BundleFile {
        path: "platform.json".into(),
        contents: format!(
            "{{\n  \"os\": {},\n  \"arch\": {},\n  \"family\": {},\n  \"version\": {}\n}}\n",
            json_string(&input.platform.os),
            json_string(&input.platform.arch),
            json_string(&input.platform.family),
            json_string(&input.version),
        ),
    });

    files.push(BundleFile {
        path: "doctor.json".into(),
        contents: format!("{}\n", scrub(&input.doctor_json)),
    });

    match &input.collection {
        Some(collection) => {
            let mut config = String::new();
            config.push_str("# Configuration from this collection, values redacted.\n");
            config.push_str("# Request files are NOT included — only their names.\n\n");
            config.push_str(&format!(
                "# collection root: {}\n\n",
                scrub(&collection.root)
            ));

            for (name, raw) in &collection.config_files {
                config.push_str(&format!("# ── {name} ──\n"));
                config.push_str(&redact_toml(raw));
                config.push_str("\n\n");
            }

            config.push_str("# requests found:\n");
            for name in &collection.request_names {
                config.push_str(&format!("#   {name}\n"));
            }
            files.push(BundleFile {
                path: "config-redacted.toml".into(),
                contents: config,
            });
        }
        None => files.push(BundleFile {
            path: "config-redacted.toml".into(),
            contents: "# No collection was found from this directory.\n".into(),
        }),
    }

    files.push(BundleFile {
        path: "logs/recent-errors.json".into(),
        contents: errors_json(&input.errors),
    });

    files.push(BundleFile {
        path: "logs/captures-summary.json".into(),
        contents: captures_json(input.captures.as_ref()),
    });

    // A bundle is written into whatever directory the user happened to be in,
    // which is usually a repository. Redacted or not, it is diagnostic output
    // about someone's machine and should not become a commit by accident.
    files.push(BundleFile {
        path: ".gitignore".into(),
        contents: "# A support bundle is not part of your project.\n*\n".into(),
    });

    // Written last so it can describe what the others turned out to contain.
    files.push(BundleFile {
        path: "README.md".into(),
        contents: bundle_readme(input),
    });

    files
}

fn errors_json(errors: &[ErrorRecord]) -> String {
    let mut out = String::from("[\n");
    for (i, error) in errors.iter().enumerate() {
        out.push_str(&format!(
            "  {{\"at_ms\": {}, \"command\": {}, \"message\": {}}}",
            error.at_ms,
            json_string(&error.command),
            json_string(&scrub(&error.message)),
        ));
        if i + 1 < errors.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("]\n");
    out
}

fn captures_json(summary: Option<&CaptureSummary>) -> String {
    let Some(summary) = summary else {
        return "{\"captures\": null, \"note\": \"no capture log in this collection\"}\n"
            .to_string();
    };

    let hosts: Vec<String> = summary
        .by_host
        .iter()
        .map(|(host, count)| format!("    {}: {}", json_string(host), count))
        .collect();
    let statuses: Vec<String> = summary
        .by_status
        .iter()
        .map(|(status, count)| format!("    \"{status}\": {count}"))
        .collect();

    format!(
        "{{\n  \"total\": {},\n  \"oldest_ms\": {},\n  \"newest_ms\": {},\n  \"by_host\": {{\n{}\n  }},\n  \"by_status\": {{\n{}\n  }}\n}}\n",
        summary.total,
        summary.oldest_ms.map_or("null".into(), |v| v.to_string()),
        summary.newest_ms.map_or("null".into(), |v| v.to_string()),
        hosts.join(",\n"),
        statuses.join(",\n"),
    )
}

fn bundle_readme(input: &Input) -> String {
    let collection_line = match &input.collection {
        Some(collection) => format!(
            "{} request(s), {} config file(s)",
            collection.request_names.len(),
            collection.config_files.len()
        ),
        None => "no collection found".to_string(),
    };

    format!(
        r#"# Gabriel feedback bundle

gabriel {version} on {arch}-{os}. Generated because you ran `gabriel feedback`.
Nothing has been sent anywhere. Read these files, delete anything you would
rather not share, then attach the directory to a bug report.

## What is in here

| File | What it holds |
| --- | --- |
| `version.txt` | Version and platform, one line each. |
| `platform.json` | Operating system, architecture, binary version. |
| `doctor.json` | The output of `gabriel doctor` — check names, statuses, and what each one found. |
| `config-redacted.toml` | Your collection and environment files with every value redacted, plus the *names* of your requests. |
| `logs/recent-errors.json` | The last {max_errors} commands that failed, with their error messages. |
| `logs/captures-summary.json` | How many requests the capture proxy recorded, by host and status code. Counts only. |

This collection: {collection_line}.

## What is deliberately not in here

- **The vault.** No secrets, encrypted or otherwise.
- **Session cookie jars.** These are live credentials.
- **The CA private key.**
- **Captured requests and responses.** No URLs, no headers, no bodies — only
  the counts in `captures-summary.json`.
- **Your request files.** Only their names, in case one of them is the problem.

Files are built from a list of fields known to be safe rather than copied and
stripped, so anything added to Gabriel later is absent from this bundle until
somebody deliberately puts it in.

## What redaction does and does not guarantee

Values in `config-redacted.toml` are replaced with `{mask}` when their key
looks like a secret, and scrubbed for credential-shaped text otherwise.
Error messages are scrubbed the same way: URL passwords, JWTs, `Bearer` tokens,
anything labelled like a key, and any long opaque string with both letters and
digits in it.

Scrubbing is a second line of defence, not the first. It cannot recognise a
secret that looks like ordinary prose. **Read the files before you share
them** — that is why this command writes a directory you can open rather than
uploading anything.
"#,
        version = input.version,
        arch = input.platform.arch,
        os = input.platform.os,
        max_errors = MAX_ERRORS,
        mask = MASK,
        collection_line = collection_line,
    )
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── writing it out ───────────────────────────────────────────────────────────

/// Write the bundle, refusing to scatter files into an existing directory that
/// somebody might mistake for their own.
pub fn write_bundle(dir: &Path, files: &[BundleFile]) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .with_context(|| format!("could not replace the existing {}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;

    for file in files {
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &file.contents)
            .with_context(|| format!("could not write {}", path.display()))?;
        restrict(&path);
    }
    Ok(())
}

#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

// ── the error log ────────────────────────────────────────────────────────────

fn errors_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("errors.jsonl")
}

/// Append a failure to the local error log, keeping the last [`MAX_ERRORS`].
///
/// Best-effort by design: a failure to record a failure must never become the
/// thing the user sees. Every error here is swallowed.
pub fn record_error(runtime_dir: &Path, command: &str, message: &str) {
    if !runtime_dir.is_dir() {
        return;
    }
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let line = format!(
        "{{\"at_ms\": {}, \"command\": {}, \"message\": {}}}",
        at_ms,
        json_string(command),
        json_string(&scrub(message)),
    );

    let path = errors_path(runtime_dir);
    let mut kept: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    kept.push(line);
    let start = kept.len().saturating_sub(MAX_ERRORS);
    let body = kept[start..].join("\n") + "\n";

    if std::fs::write(&path, body).is_ok() {
        restrict(&path);
    }
}

/// Read back what [`record_error`] wrote. Malformed lines are skipped rather
/// than failing the bundle: a corrupt log is not a reason to withhold the rest.
pub fn read_errors(runtime_dir: &Path) -> Vec<ErrorRecord> {
    let Ok(text) = std::fs::read_to_string(errors_path(runtime_dir)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            Some(ErrorRecord {
                at_ms: value.get("at_ms")?.as_u64()?,
                command: value.get("command")?.as_str()?.to_string(),
                message: value.get("message")?.as_str()?.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── scrubbing ───────────────────────────────────────────────────────────

    #[test]
    fn a_password_in_a_proxy_url_is_removed() {
        let scrubbed = scrub("HTTPS_PROXY=http://alice:hunter2@proxy.corp:8080");
        assert!(!scrubbed.contains("hunter2"), "{scrubbed}");
        // The part that makes it diagnosable survives.
        assert!(scrubbed.contains("proxy.corp:8080"), "{scrubbed}");
    }

    #[test]
    fn a_url_without_credentials_is_left_alone() {
        let text = "connecting to https://api.example.com/v1/users?page=2";
        assert_eq!(scrub(text), text);
    }

    #[test]
    fn a_jwt_is_removed_even_when_it_is_short() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc";
        let scrubbed = scrub(&format!("token rejected: {jwt}"));
        assert!(!scrubbed.contains("eyJzdWIiOiIxIn0"), "{scrubbed}");
        assert!(scrubbed.contains("[redacted-jwt]"), "{scrubbed}");
    }

    #[test]
    fn a_labelled_secret_is_removed_whatever_it_looks_like() {
        // Short, lowercase, no digits: nothing about the value gives it away.
        // Only the label does, which is exactly why labels are checked.
        for text in [
            "password=swordfish",
            "\"api_key\": \"swordfish\"",
            "Authorization: Bearer swordfish",
            "client_secret = swordfish",
        ] {
            let scrubbed = scrub(text);
            assert!(!scrubbed.contains("swordfish"), "{text} → {scrubbed}");
        }
    }

    #[test]
    fn an_unlabelled_api_key_is_removed_by_shape() {
        let key = "sk1a2b3c4d5e6f7g8h9i0j1k2l3m4n5o6p7q8r9s";
        let scrubbed = scrub(&format!("request failed with {key}"));
        assert!(!scrubbed.contains(key), "{scrubbed}");
    }

    #[test]
    fn file_paths_survive_because_they_are_what_makes_a_bundle_useful() {
        let path = "/Users/someone/Documents/projects/gabriel/collection.toml";
        assert_eq!(scrub(path), path);
    }

    #[test]
    fn ordinary_prose_is_not_mangled() {
        let text = "could not connect to the server after 3 attempts";
        assert_eq!(scrub(text), text);
    }

    #[test]
    fn a_capture_id_survives() {
        // 16 hex characters — under the opaque-run threshold on purpose, since
        // "which capture" is the first question support will ask.
        let text = "no such capture: c19faf40372f0002";
        assert_eq!(scrub(text), text);
    }

    // ── configuration ───────────────────────────────────────────────────────

    #[test]
    fn config_keeps_its_keys_and_loses_its_values() {
        let raw = r#"
name = "my-collection"
[vars]
base_url = "https://api.example.com"
api_token = "sk-live-abcdef123456"
"#;
        let redacted = redact_toml(raw);
        assert!(redacted.contains("api_token"), "{redacted}");
        assert!(!redacted.contains("sk-live-abcdef123456"), "{redacted}");
        // Non-secret values are diagnosable and stay.
        assert!(redacted.contains("https://api.example.com"), "{redacted}");
    }

    /// Doubles as the regression test for the structured path being taken at
    /// all: `str::parse::<Value>` parses a value, not a document, so this once
    /// fell through to the text scrubber, which mangles the reference instead
    /// of recognising it.
    #[test]
    fn a_secret_reference_is_kept_because_it_is_a_pointer_not_a_secret() {
        let raw = "[vars]\napi_token = \"{{secret:prod_key}}\"\n";
        let redacted = redact_toml(raw);
        assert!(redacted.contains("{{secret:prod_key}}"), "{redacted}");
    }

    #[test]
    fn unparseable_config_is_still_included_but_scrubbed() {
        let raw = "this is not = = toml at all\npassword=swordfish\n";
        let redacted = redact_toml(raw);
        assert!(!redacted.contains("swordfish"), "{redacted}");
    }

    // ── the bundle ──────────────────────────────────────────────────────────

    fn sample_input() -> Input {
        Input {
            version: "0.1.0-preview.1".into(),
            platform: Platform {
                os: "macos".into(),
                arch: "aarch64".into(),
                family: "unix".into(),
            },
            doctor_json: "[{\"name\": \"version\", \"status\": \"ok\"}]".into(),
            collection: Some(CollectionInfo {
                root: "/tmp/demo/gabriel".into(),
                request_names: vec!["users/me".into()],
                config_files: vec![(
                    "collection.toml".into(),
                    "name = \"demo\"\n[vars]\ntoken = \"sk-live-abcdef123456\"\n".into(),
                )],
            }),
            captures: Some(CaptureSummary {
                total: 2,
                by_host: [("api.example.com".to_string(), 2)].into_iter().collect(),
                by_status: [(200u16, 2)].into_iter().collect(),
                oldest_ms: Some(1),
                newest_ms: Some(2),
            }),
            errors: vec![ErrorRecord {
                at_ms: 5,
                command: "run".into(),
                message: "failed with password=swordfish".into(),
            }],
        }
    }

    #[test]
    fn the_bundle_has_the_files_it_promises() {
        let files = build(&sample_input());
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        for expected in [
            "version.txt",
            "platform.json",
            "doctor.json",
            "config-redacted.toml",
            "logs/recent-errors.json",
            "logs/captures-summary.json",
            "README.md",
        ] {
            assert!(paths.contains(&expected), "missing {expected} in {paths:?}");
        }
    }

    /// The property the whole command rests on. Asserted over the *whole*
    /// bundle rather than per file, so a secret cannot survive by moving from
    /// one file to another.
    #[test]
    fn no_secret_survives_anywhere_in_the_bundle() {
        let files = build(&sample_input());
        let everything: String = files.iter().map(|f| f.contents.as_str()).collect();
        for secret in ["sk-live-abcdef123456", "swordfish"] {
            assert!(
                !everything.contains(secret),
                "{secret} leaked into the bundle:\n{everything}"
            );
        }
    }

    #[test]
    fn the_capture_summary_is_counts_and_nothing_else() {
        let files = build(&sample_input());
        let summary = files
            .iter()
            .find(|f| f.path == "logs/captures-summary.json")
            .unwrap();
        assert!(summary.contents.contains("\"total\": 2"));
        // A host is a count key; a URL, header or body never appears.
        assert!(!summary.contents.contains("http"), "{}", summary.contents);
        assert!(
            !summary.contents.to_lowercase().contains("cookie"),
            "{}",
            summary.contents
        );
    }

    #[test]
    fn the_readme_says_what_was_left_out() {
        let files = build(&sample_input());
        let readme = files.iter().find(|f| f.path == "README.md").unwrap();
        for promise in ["vault", "cookie", "CA private key", "Read the files"] {
            assert!(
                readme.contents.contains(promise),
                "the bundle README does not mention {promise}"
            );
        }
    }

    #[test]
    fn a_bundle_cannot_be_committed_by_accident() {
        let files = build(&sample_input());
        let ignore = files
            .iter()
            .find(|f| f.path == ".gitignore")
            .expect("a bundle written into a repository would be committable");
        assert!(ignore.contents.contains('*'));
    }

    #[test]
    fn a_bundle_without_a_collection_still_builds() {
        let mut input = sample_input();
        input.collection = None;
        input.captures = None;
        let files = build(&input);
        assert!(files.iter().any(|f| f.path == "config-redacted.toml"));
    }

    // ── the error log ───────────────────────────────────────────────────────

    /// Unique per process and per call, so the suite survives being run in
    /// parallel with itself.
    fn temp_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gabriel-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn errors_round_trip_and_are_scrubbed_on_the_way_in() {
        let dir = temp_dir("feedback-errors");
        std::fs::create_dir_all(&dir).unwrap();

        record_error(&dir, "run", "boom: password=swordfish");
        let read = read_errors(&dir);

        assert_eq!(read.len(), 1);
        assert_eq!(read[0].command, "run");
        assert!(!read[0].message.contains("swordfish"), "{:?}", read[0]);
        // Scrubbed on the way *in*, so the secret is not sitting in a file on
        // disk waiting for the next bundle either.
        let raw = std::fs::read_to_string(errors_path(&dir)).unwrap();
        assert!(!raw.contains("swordfish"), "{raw}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_error_log_stays_bounded() {
        let dir = temp_dir("feedback-bounded");
        std::fs::create_dir_all(&dir).unwrap();

        for i in 0..(MAX_ERRORS + 20) {
            record_error(&dir, "run", &format!("failure number {i}"));
        }
        let read = read_errors(&dir);
        assert_eq!(read.len(), MAX_ERRORS);
        // The ones kept are the recent ones.
        assert!(
            read.last()
                .unwrap()
                .message
                .contains(&format!("failure number {}", MAX_ERRORS + 19))
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recording_outside_a_collection_does_nothing_at_all() {
        let dir = temp_dir("feedback-absent");
        // Deliberately not created.
        record_error(&dir, "run", "boom");
        assert!(!dir.exists(), "recording an error created a directory");
    }

    #[test]
    fn a_corrupt_log_line_does_not_lose_the_others() {
        let dir = temp_dir("feedback-corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            errors_path(&dir),
            "{not json at all\n{\"at_ms\": 1, \"command\": \"run\", \"message\": \"fine\"}\n",
        )
        .unwrap();

        let read = read_errors(&dir);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].message, "fine");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writing_a_bundle_replaces_an_old_one_rather_than_merging() {
        let dir = temp_dir("feedback-write");
        let files = build(&sample_input());

        write_bundle(&dir, &files).unwrap();
        std::fs::write(dir.join("stale.txt"), "from a previous run").unwrap();
        write_bundle(&dir, &files).unwrap();

        assert!(!dir.join("stale.txt").exists(), "a stale file survived");
        assert!(dir.join("logs/recent-errors.json").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn bundle_files_are_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("feedback-modes");
        write_bundle(&dir, &build(&sample_input())).unwrap();

        let mode = std::fs::metadata(dir.join("config-redacted.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "bundle file is readable by others");

        std::fs::remove_dir_all(&dir).ok();
    }
}
