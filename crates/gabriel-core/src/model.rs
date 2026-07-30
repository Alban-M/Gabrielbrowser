//! The on-disk request format.
//!
//! A request is one TOML file. TOML was chosen over a bespoke format (Bruno's
//! `.bru`, Postman's JSON) deliberately: it is already diffable, already
//! reviewable in a pull request, already parseable by every language, and it
//! costs us no parser of our own to maintain. The format is the product's
//! anti-lock-in promise, so it stays boring on purpose.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A single request as it lives on disk.
///
/// Field order here is also the serialization order, which is what a reviewer
/// sees in a diff — method and URL first, noise last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestSpec {
    /// Human label. Defaults to the file stem when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default = "default_method")]
    pub method: String,

    pub url: String,

    #[serde(default, skip_serializing_if = "FieldMap::is_empty")]
    pub headers: FieldMap,

    #[serde(default, skip_serializing_if = "FieldMap::is_empty")]
    pub query: FieldMap,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,

    #[serde(default, skip_serializing_if = "Settings::is_default")]
    pub settings: Settings,

    /// Values pulled out of the response and bound as variables for later
    /// requests in the same run. This is what makes multi-step flows work
    /// without copy-paste between calls.
    #[serde(default, rename = "capture", skip_serializing_if = "Vec::is_empty")]
    pub captures: Vec<VarCapture>,

    #[serde(default, rename = "assert", skip_serializing_if = "Vec::is_empty")]
    pub asserts: Vec<Assertion>,

    /// Provenance. Set when the request was promoted from captured traffic
    /// rather than hand-written, so a reader knows where it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

fn default_method() -> String {
    "GET".to_string()
}

impl RequestSpec {
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: None,
            description: None,
            method: method.into().to_uppercase(),
            url: url.into(),
            headers: FieldMap::default(),
            query: FieldMap::default(),
            auth: None,
            body: None,
            settings: Settings::default(),
            captures: Vec::new(),
            asserts: Vec::new(),
            origin: None,
        }
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.url)
    }
}

/// Headers and query parameters: an ordered-by-key map where one key may carry
/// several values (`Set-Cookie`, repeated `?tag=` params).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FieldMap(pub BTreeMap<String, FieldValue>);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldValue {
    One(String),
    Many(Vec<String>),
}

impl FieldMap {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        match self.0.get_mut(&key) {
            Some(FieldValue::One(existing)) => {
                let first = existing.clone();
                self.0.insert(key, FieldValue::Many(vec![first, value]));
            }
            Some(FieldValue::Many(list)) => list.push(value),
            None => {
                self.0.insert(key, FieldValue::One(value));
            }
        }
    }

    /// Set, replacing any existing values for the key.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), FieldValue::One(value.into()));
    }

    pub fn get_first(&self, key: &str) -> Option<&str> {
        // Header lookups are case-insensitive; query lookups happen to be
        // case-sensitive but callers pass the exact key there.
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| match v {
                FieldValue::One(s) => s.as_str(),
                FieldValue::Many(list) => list.first().map(String::as_str).unwrap_or(""),
            })
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.0.keys().any(|k| k.eq_ignore_ascii_case(key))
    }

    pub fn remove(&mut self, key: &str) {
        let found: Vec<String> = self
            .0
            .keys()
            .filter(|k| k.eq_ignore_ascii_case(key))
            .cloned()
            .collect();
        for key in found {
            self.0.remove(&key);
        }
    }

    /// Flatten to `(key, value)` pairs, expanding multi-valued keys.
    pub fn iter_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().flat_map(|(k, v)| match v {
            FieldValue::One(s) => vec![(k.as_str(), s.as_str())],
            FieldValue::Many(list) => list.iter().map(|s| (k.as_str(), s.as_str())).collect(),
        })
    }
}

impl FromIterator<(String, String)> for FieldMap {
    fn from_iter<T: IntoIterator<Item = (String, String)>>(iter: T) -> Self {
        let mut map = FieldMap::default();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

/// Authentication. `Session` is the one that matters strategically: it means
/// "replay this using the cookies the browser already holds", which is the
/// step Postman and friends structurally cannot do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    None,
    /// Take whatever the collection or parent folder declares.
    Inherit,
    /// Reuse a captured browser session's cookies (and, when present, its
    /// Authorization header) for this host.
    Session {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    Bearer {
        token: String,
    },
    Basic {
        username: String,
        password: String,
    },
    ApiKey {
        key: String,
        value: String,
        #[serde(default)]
        location: ApiKeyLocation,
    },
    OAuth2(OAuth2),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyLocation {
    #[default]
    Header,
    Query,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuth2 {
    pub grant: OAuth2Grant,
    pub token_url: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    /// Send credentials in the POST body instead of a Basic header. Some
    /// providers only accept one or the other.
    #[serde(default)]
    pub credentials_in_body: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuth2Grant {
    ClientCredentials,
    Password,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    None,
    /// Stored as raw text, not as parsed TOML, so that the JSON in the file is
    /// the JSON on the wire — byte for byte, templates included.
    Json {
        content: String,
    },
    Text {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
    },
    Form {
        fields: BTreeMap<String, String>,
    },
    GraphQl {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variables: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_name: Option<String>,
    },
    /// Path to a file on disk, relative to the collection root.
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    #[serde(default = "default_max_redirects")]
    pub max_redirects: usize,
    /// Set to false only against a host with a self-signed certificate you
    /// control. Off-by-default footguns stay off by default.
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// Client certificate (mTLS), by vault key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert: Option<String>,
}

fn default_timeout() -> u64 {
    30_000
}
fn default_true() -> bool {
    true
}
fn default_max_redirects() -> usize {
    10
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout_ms: default_timeout(),
            follow_redirects: true,
            max_redirects: default_max_redirects(),
            verify_tls: true,
            proxy: None,
            client_cert: None,
        }
    }
}

impl Settings {
    pub fn is_default(&self) -> bool {
        self == &Settings::default()
    }
}

/// Bind a value out of the response to a variable name for later requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VarCapture {
    /// Variable name to bind.
    pub var: String,
    #[serde(default)]
    pub from: CaptureSource,
    /// JSON path (`data.items[0].id`) for body captures, or the header/cookie
    /// name for those sources. Ignored for `status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSource {
    #[default]
    Body,
    Header,
    Cookie,
    Status,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assertion {
    #[serde(default)]
    pub target: AssertTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub op: AssertOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<toml::Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertTarget {
    #[default]
    Status,
    Header,
    Body,
    DurationMs,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertOp {
    #[default]
    Eq,
    Ne,
    Lt,
    Gt,
    Contains,
    Exists,
    Missing,
}

/// Where a request came from, when it wasn't typed by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    /// Capture id this was promoted from.
    pub capture: String,
    /// Epoch milliseconds.
    pub promoted_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_through_toml() {
        let mut spec = RequestSpec::new("post", "{{base_url}}/users");
        spec.name = Some("Create user".into());
        spec.headers.set("Content-Type", "application/json");
        spec.headers.set("Accept", "application/json");
        spec.auth = Some(Auth::Bearer { token: "{{secret:api_token}}".into() });
        spec.body = Some(Body::Json { content: "{\n  \"name\": \"ada\"\n}".into() });
        spec.captures.push(VarCapture {
            var: "user_id".into(),
            from: CaptureSource::Body,
            path: Some("id".into()),
        });

        let text = toml::to_string_pretty(&spec).expect("serialize");
        let back: RequestSpec = toml::from_str(&text).expect("deserialize");
        assert_eq!(spec, back);
        // The body must survive as literal text, not as re-encoded TOML.
        assert!(text.contains("\"name\": \"ada\""), "body mangled:\n{text}");
    }

    #[test]
    fn minimal_request_needs_only_a_url() {
        let spec: RequestSpec = toml::from_str(r#"url = "https://example.com""#).unwrap();
        assert_eq!(spec.method, "GET");
        assert!(spec.settings.is_default());
        assert!(spec.headers.is_empty());
    }

    #[test]
    fn default_settings_are_omitted_from_output() {
        let spec = RequestSpec::new("GET", "https://example.com");
        let text = toml::to_string_pretty(&spec).unwrap();
        assert!(!text.contains("timeout_ms"), "noise in output:\n{text}");
    }

    #[test]
    fn field_map_holds_repeated_keys() {
        let mut map = FieldMap::default();
        map.insert("Set-Cookie", "a=1");
        map.insert("Set-Cookie", "b=2");
        assert_eq!(map.len(), 1);
        assert_eq!(map.iter_pairs().count(), 2);
        assert_eq!(map.get_first("set-cookie"), Some("a=1"));
    }

    #[test]
    fn field_map_lookup_ignores_header_case() {
        let mut map = FieldMap::default();
        map.set("Content-Type", "application/json");
        assert!(map.contains_key("content-type"));
        map.remove("CONTENT-TYPE");
        assert!(map.is_empty());
    }
}
