//! Cookie jars, keyed by session.
//!
//! This is the mechanism behind "replay it with the page's session". The proxy
//! records `Set-Cookie` headers as the developer browses; the engine sends the
//! matching ones back when a captured request is replayed. No re-login, no
//! copied token.
//!
//! Cookie scoping is a security boundary, not a convenience: getting
//! domain-matching wrong here would send a session cookie for one site to
//! another. The rules below follow RFC 6265 §5.1.3 (domain) and §5.1.4 (path).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Host or registrable domain this cookie belongs to, without a leading dot.
    pub domain: String,
    #[serde(default = "root_path")]
    pub path: String,
    /// True when the cookie had no `Domain` attribute: it goes back only to the
    /// exact host that set it.
    #[serde(default)]
    pub host_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    /// Epoch milliseconds; `None` means a session cookie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<u64>,
}

fn root_path() -> String {
    "/".to_string()
}

impl Cookie {
    /// Parse one `Set-Cookie` value as seen from `request_host` / `request_path`.
    ///
    /// Returns `None` when the header is malformed or when the `Domain`
    /// attribute tries to claim a domain the origin has no right to set.
    pub fn parse(header: &str, request_host: &str, request_path: &str) -> Option<Self> {
        let mut parts = header.split(';');
        let (name, value) = parts.next()?.split_once('=')?;
        let name = name.trim();
        if name.is_empty() {
            return None;
        }

        let request_host = request_host.trim().to_ascii_lowercase();
        let mut cookie = Cookie {
            name: name.to_string(),
            value: value.trim().to_string(),
            domain: request_host.clone(),
            path: default_path(request_path),
            host_only: true,
            secure: false,
            http_only: false,
            expires_ms: None,
        };

        let mut max_age: Option<i64> = None;
        for attr in parts {
            let (key, val) = match attr.split_once('=') {
                Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim()),
                None => (attr.trim().to_ascii_lowercase(), ""),
            };
            match key.as_str() {
                "domain" => {
                    let claimed = val.trim_start_matches('.').to_ascii_lowercase();
                    // A site may widen a cookie to its own parent domain, and
                    // nothing else. `evil.test` cannot set one for `bank.test`,
                    // and nobody may set one for a bare TLD.
                    if claimed.is_empty()
                        || !claimed.contains('.')
                        || !domain_matches(&request_host, &claimed)
                    {
                        return None;
                    }
                    cookie.domain = claimed;
                    cookie.host_only = false;
                }
                "path" if val.starts_with('/') => cookie.path = val.to_string(),
                "secure" => cookie.secure = true,
                "httponly" => cookie.http_only = true,
                "max-age" => max_age = val.parse().ok(),
                "expires" => cookie.expires_ms = parse_http_date(val),
                _ => {}
            }
        }

        // Max-Age wins over Expires per RFC 6265 §5.2.2.
        if let Some(seconds) = max_age {
            cookie.expires_ms =
                Some((gabriel_core::now_ms() as i64 + seconds * 1000).max(0) as u64);
        }

        Some(cookie)
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_ms.is_some_and(|expiry| expiry <= now_ms)
    }

    /// Whether this cookie should be sent to the given request.
    pub fn matches(&self, host: &str, path: &str, is_secure: bool) -> bool {
        if self.secure && !is_secure {
            return false;
        }
        let host = host.to_ascii_lowercase();
        let host_ok = if self.host_only {
            host == self.domain
        } else {
            domain_matches(&host, &self.domain)
        };
        host_ok && path_matches(path, &self.path)
    }

    /// The key that identifies "the same cookie" for replacement purposes.
    fn identity(&self) -> (String, String, String) {
        (self.name.clone(), self.domain.clone(), self.path.clone())
    }
}

/// RFC 6265 §5.1.3: `host` matches `domain` if they are equal, or if `host`
/// ends with `.domain`. The dot is what stops `notbank.test` from matching
/// `bank.test`.
fn domain_matches(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    host.len() > domain.len()
        && host.ends_with(domain)
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}

/// RFC 6265 §5.1.4.
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

/// RFC 6265 §5.1.4 default-path: the request path up to its last `/`.
fn default_path(request_path: &str) -> String {
    if !request_path.starts_with('/') {
        return "/".to_string();
    }
    match request_path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => request_path[..index].to_string(),
    }
}

/// Parse the IMF-fixdate form used by `Expires` (`Wed, 21 Oct 2026 07:28:00 GMT`).
/// Other legacy formats are treated as a session cookie rather than guessed at.
fn parse_http_date(value: &str) -> Option<u64> {
    let value = value.trim();
    let rest = value.split_once(", ").map(|(_, r)| r).unwrap_or(value);
    let mut fields = rest.split_whitespace();
    let day: i64 = fields.next()?.parse().ok()?;
    let month = match fields.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = fields.next()?.parse().ok()?;
    let time = fields.next()?;
    let mut hms = time.split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    let second: i64 = hms.next()?.parse().ok()?;

    let days = gabriel_core::days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    Some((secs.max(0) as u64) * 1000)
}

/// Named cookie jars, one per session (a browser profile, a Space, a persona).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStore {
    #[serde(default)]
    sessions: BTreeMap<String, Vec<Cookie>>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

pub const DEFAULT_SESSION: &str = "default";

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from disk, or start empty if the file isn't there yet.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let path = path.as_ref().to_path_buf();
        let mut store = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SessionStore::default(),
            Err(e) => return Err(e),
        };
        store.path = Some(path);
        Ok(store)
    }

    pub fn save(&self) -> Result<(), std::io::Error> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        write_private(path, text.as_bytes())
    }

    pub fn set_path(&mut self, path: impl Into<PathBuf>) {
        self.path = Some(path.into());
    }

    pub fn names(&self) -> Vec<&str> {
        self.sessions.keys().map(String::as_str).collect()
    }

    pub fn cookie_count(&self, session: &str) -> usize {
        self.sessions.get(session).map(Vec::len).unwrap_or(0)
    }

    pub fn clear(&mut self, session: &str) -> usize {
        self.sessions.remove(session).map(|c| c.len()).unwrap_or(0)
    }

    /// Record a cookie, replacing any same name/domain/path already held.
    pub fn insert(&mut self, session: &str, cookie: Cookie) {
        let jar = self.sessions.entry(session.to_string()).or_default();
        let identity = cookie.identity();
        jar.retain(|existing| existing.identity() != identity);
        // A cookie with an expiry in the past is a deletion instruction.
        if !cookie.is_expired(gabriel_core::now_ms()) {
            jar.push(cookie);
        }
    }

    /// Absorb every `Set-Cookie` from a response.
    pub fn record_set_cookies<'a>(
        &mut self,
        session: &str,
        headers: impl IntoIterator<Item = &'a str>,
        request_host: &str,
        request_path: &str,
    ) -> usize {
        let mut recorded = 0;
        for header in headers {
            if let Some(cookie) = Cookie::parse(header, request_host, request_path) {
                self.insert(session, cookie);
                recorded += 1;
            }
        }
        recorded
    }

    /// The `Cookie` header value for a request, or `None` when nothing matches.
    pub fn cookie_header(
        &self,
        session: &str,
        host: &str,
        path: &str,
        is_secure: bool,
    ) -> Option<String> {
        let jar = self.sessions.get(session)?;
        let now = gabriel_core::now_ms();
        let mut matching: Vec<&Cookie> = jar
            .iter()
            .filter(|c| !c.is_expired(now) && c.matches(host, path, is_secure))
            .collect();
        if matching.is_empty() {
            return None;
        }
        // RFC 6265 §5.4: longer paths first.
        matching.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        Some(
            matching
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// Drop expired cookies. Returns how many were removed.
    pub fn prune(&mut self) -> usize {
        let now = gabriel_core::now_ms();
        let mut removed = 0;
        for jar in self.sessions.values_mut() {
            let before = jar.len();
            jar.retain(|c| !c.is_expired(now));
            removed += before - jar.len();
        }
        removed
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_cookie_as_host_only() {
        let cookie = Cookie::parse("sid=abc123", "app.test", "/login").unwrap();
        assert_eq!(cookie.name, "sid");
        assert_eq!(cookie.value, "abc123");
        assert_eq!(cookie.domain, "app.test");
        assert!(cookie.host_only);
        assert_eq!(cookie.path, "/");
    }

    #[test]
    fn default_path_is_the_directory_of_the_request() {
        let cookie = Cookie::parse("a=1", "app.test", "/admin/users/7").unwrap();
        assert_eq!(cookie.path, "/admin/users");
    }

    #[test]
    fn parses_attributes() {
        let cookie = Cookie::parse(
            "sid=abc; Domain=.app.test; Path=/admin; Secure; HttpOnly",
            "www.app.test",
            "/",
        )
        .unwrap();
        assert_eq!(cookie.domain, "app.test");
        assert!(!cookie.host_only);
        assert_eq!(cookie.path, "/admin");
        assert!(cookie.secure && cookie.http_only);
    }

    #[test]
    fn a_site_cannot_set_a_cookie_for_another_domain() {
        assert!(Cookie::parse("sid=abc; Domain=bank.test", "evil.test", "/").is_none());
    }

    #[test]
    fn nobody_can_set_a_cookie_for_a_bare_tld() {
        assert!(Cookie::parse("sid=abc; Domain=test", "evil.test", "/").is_none());
        assert!(Cookie::parse("sid=abc; Domain=.com", "evil.com", "/").is_none());
    }

    #[test]
    fn a_subdomain_may_widen_to_its_parent() {
        let cookie = Cookie::parse("sid=abc; Domain=app.test", "api.app.test", "/").unwrap();
        assert_eq!(cookie.domain, "app.test");
        assert!(cookie.matches("other.app.test", "/", false));
    }

    #[test]
    fn host_only_cookies_do_not_leak_to_subdomains() {
        let cookie = Cookie::parse("sid=abc", "app.test", "/").unwrap();
        assert!(cookie.matches("app.test", "/", false));
        assert!(!cookie.matches("api.app.test", "/", false));
    }

    #[test]
    fn domain_cookies_do_not_leak_to_lookalike_hosts() {
        let cookie = Cookie::parse("sid=abc; Domain=bank.test", "bank.test", "/").unwrap();
        assert!(!cookie.matches("notbank.test", "/", false));
        assert!(!cookie.matches("bank.test.evil.com", "/", false));
        assert!(cookie.matches("secure.bank.test", "/", false));
    }

    #[test]
    fn secure_cookies_are_withheld_from_plaintext_requests() {
        let cookie = Cookie::parse("sid=abc; Secure", "app.test", "/").unwrap();
        assert!(cookie.matches("app.test", "/", true));
        assert!(!cookie.matches("app.test", "/", false));
    }

    #[test]
    fn path_scoping_follows_the_spec() {
        let cookie = Cookie::parse("a=1; Path=/admin", "app.test", "/").unwrap();
        assert!(cookie.matches("app.test", "/admin", false));
        assert!(cookie.matches("app.test", "/admin/users", false));
        assert!(!cookie.matches("app.test", "/administrator", false));
        assert!(!cookie.matches("app.test", "/", false));
    }

    #[test]
    fn max_age_sets_an_expiry_and_zero_deletes() {
        let live = Cookie::parse("a=1; Max-Age=3600", "app.test", "/").unwrap();
        assert!(live.expires_ms.unwrap() > gabriel_core::now_ms());

        let dead = Cookie::parse("a=1; Max-Age=0", "app.test", "/").unwrap();
        assert!(dead.is_expired(gabriel_core::now_ms()));
    }

    #[test]
    fn parses_an_expires_date() {
        let cookie = Cookie::parse(
            "a=1; Expires=Wed, 21 Oct 2026 07:28:00 GMT",
            "app.test",
            "/",
        )
        .unwrap();
        assert_eq!(
            gabriel_core::format_iso8601(cookie.expires_ms.unwrap()),
            "2026-10-21T07:28:00.000Z"
        );
    }

    #[test]
    fn the_jar_builds_a_cookie_header() {
        let mut store = SessionStore::new();
        store.record_set_cookies(
            "work",
            ["sid=abc; Path=/", "theme=dark; Path=/"],
            "app.test",
            "/",
        );
        let header = store
            .cookie_header("work", "app.test", "/dashboard", true)
            .unwrap();
        assert!(header.contains("sid=abc"));
        assert!(header.contains("theme=dark"));
    }

    #[test]
    fn jars_are_isolated_between_sessions() {
        let mut store = SessionStore::new();
        store.record_set_cookies("work", ["sid=work"], "app.test", "/");
        store.record_set_cookies("personal", ["sid=personal"], "app.test", "/");

        assert_eq!(
            store.cookie_header("work", "app.test", "/", true).unwrap(),
            "sid=work"
        );
        assert_eq!(
            store
                .cookie_header("personal", "app.test", "/", true)
                .unwrap(),
            "sid=personal"
        );
    }

    #[test]
    fn a_cookie_for_one_site_is_never_sent_to_another() {
        let mut store = SessionStore::new();
        store.record_set_cookies("work", ["sid=secret"], "bank.test", "/");
        assert!(
            store
                .cookie_header("work", "evil.test", "/", true)
                .is_none()
        );
    }

    #[test]
    fn resetting_a_cookie_replaces_rather_than_duplicates() {
        let mut store = SessionStore::new();
        store.record_set_cookies("work", ["sid=old"], "app.test", "/");
        store.record_set_cookies("work", ["sid=new"], "app.test", "/");
        assert_eq!(store.cookie_count("work"), 1);
        assert_eq!(
            store.cookie_header("work", "app.test", "/", true).unwrap(),
            "sid=new"
        );
    }

    #[test]
    fn an_expired_set_cookie_removes_the_cookie() {
        let mut store = SessionStore::new();
        store.record_set_cookies("work", ["sid=abc"], "app.test", "/");
        store.record_set_cookies("work", ["sid=; Max-Age=0"], "app.test", "/");
        assert_eq!(store.cookie_count("work"), 0);
    }

    #[test]
    fn longer_paths_are_sent_first() {
        let mut store = SessionStore::new();
        store.record_set_cookies("s", ["a=root; Path=/"], "app.test", "/");
        store.record_set_cookies("s", ["b=deep; Path=/admin"], "app.test", "/");
        let header = store
            .cookie_header("s", "app.test", "/admin/x", true)
            .unwrap();
        assert!(header.starts_with("b=deep"), "{header}");
    }

    #[test]
    fn pruning_removes_only_the_expired() {
        let mut store = SessionStore::new();
        store.record_set_cookies("s", ["live=1; Max-Age=3600"], "app.test", "/");
        store.insert(
            "s",
            Cookie {
                name: "stale".into(),
                value: "1".into(),
                domain: "app.test".into(),
                path: "/".into(),
                host_only: true,
                secure: false,
                http_only: false,
                // Already expired, inserted directly: `record_set_cookies`
                // would (correctly) drop it on the way in.
                expires_ms: Some(1),
            },
        );
        // `insert` refuses to store an already-expired cookie, which is itself
        // the deletion path — so the jar holds only the live one.
        assert_eq!(store.cookie_count("s"), 1);
        assert_eq!(store.prune(), 0);
        assert_eq!(
            store.cookie_header("s", "app.test", "/", false).as_deref(),
            Some("live=1")
        );
    }

    #[test]
    fn a_session_cookie_never_expires_on_its_own() {
        let mut store = SessionStore::new();
        store.record_set_cookies("s", ["sid=abc"], "app.test", "/");
        assert_eq!(store.prune(), 0);
        assert_eq!(store.cookie_count("s"), 1);
    }

    #[test]
    fn setting_a_path_makes_the_store_persist_there() {
        let dir = std::env::temp_dir().join(format!("gabriel-setpath-{}", gabriel_core::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.json");

        let mut store = SessionStore::new();
        // Without a path, save is a no-op rather than an error.
        store.record_set_cookies("s", ["sid=abc"], "app.test", "/");
        store.save().unwrap();
        assert!(!path.exists());

        store.set_path(&path);
        store.save().unwrap();
        assert!(path.exists(), "set_path should make save write there");
    }

    #[test]
    fn the_store_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("gabriel-session-{}", gabriel_core::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.json");

        let mut store = SessionStore::load(&path).unwrap();
        store.record_set_cookies("work", ["sid=abc"], "app.test", "/");
        store.save().unwrap();

        let reloaded = SessionStore::load(&path).unwrap();
        assert_eq!(
            reloaded
                .cookie_header("work", "app.test", "/", true)
                .unwrap(),
            "sid=abc"
        );
    }
}
