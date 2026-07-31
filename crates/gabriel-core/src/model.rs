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
    /// Spelled `oauth2` in a file. Without this rename, serde's snake_case rule
    /// derives `o_auth2` from the variant name — a spelling no one would write
    /// and every provider's documentation contradicts.
    #[serde(rename = "oauth2")]
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
    /// Where the browser is sent to log in. Required for the authorization-code
    /// grant and ignored by the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorize_url: Option<String>,
    /// Loopback redirect the provider must have registered, e.g.
    /// `http://127.0.0.1:8765/callback`. A fixed port is used when the provider
    /// requires an exact match; otherwise any free port is taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
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
    /// Authorization code with PKCE (RFC 7636). The flow runs a browser and a
    /// loopback listener, so it is started explicitly with `gabriel auth login`
    /// rather than in the middle of a request.
    AuthorizationCode,
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
    /// Client certificate for mTLS, as a PEM bundle holding both the
    /// certificate and its private key.
    ///
    /// Two forms are accepted. `"{{secret:name}}"` pulls the PEM out of the
    /// vault, so the private key never sits on disk in the clear; anything else
    /// is treated as a path relative to the collection root, the way
    /// `curl --cert` takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert: Option<String>,
    /// Override what replaying this request does to the world.
    ///
    /// Method is a good guess and wrong in one common direction: a `POST` used
    /// for a search or a report is safe to repeat, and being asked to confirm it
    /// every time would train people to confirm without reading — which is worse
    /// than not asking. Declaring `effect = "read"` says *I checked*.
    ///
    /// It can only be set by a person editing the file. Promotion never writes
    /// it, because a capture cannot know whether the endpoint it saw was a
    /// search or a purchase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<Effect>,
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
            effect: None,
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
        spec.auth = Some(Auth::Bearer {
            token: "{{secret:api_token}}".into(),
        });
        spec.body = Some(Body::Json {
            content: "{\n  \"name\": \"ada\"\n}".into(),
        });
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
    fn an_oauth2_block_is_spelled_the_way_documentation_spells_it() {
        let spec: RequestSpec = toml::from_str(
            r#"
            url = "https://api.test/me"
            [auth]
            type = "oauth2"
            grant = "authorization_code"
            authorize_url = "https://auth.test/authorize"
            token_url = "https://auth.test/token"
            client_id = "abc"
            "#,
        )
        .expect("`type = \"oauth2\"` must parse");

        let Some(Auth::OAuth2(config)) = &spec.auth else {
            panic!("expected an OAuth2 block, got {:?}", spec.auth);
        };
        assert_eq!(config.grant, OAuth2Grant::AuthorizationCode);
        assert_eq!(
            config.authorize_url.as_deref(),
            Some("https://auth.test/authorize")
        );

        // And it round-trips back to the same spelling.
        let text = toml::to_string_pretty(&spec).unwrap();
        assert!(
            text.contains("type = \"oauth2\""),
            "wrote the wrong spelling:\n{text}"
        );
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

/// What running a request does to the world.
///
/// Replay is the product's whole proposition, and it is only safe for
/// operations that can be repeated. A captured `GET` costs nothing to run
/// again; a captured payment charges the card twice. Gabriel has been safe so
/// far by accident — people replay reads while debugging — and accident is not
/// a safety model.
///
/// Derived from RFC 9110 §9.2: *safe* methods do not change state, *idempotent*
/// methods may be repeated without additional effect beyond the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Safe: reads nothing into existence. Free to repeat.
    Read,
    /// Changes state, but repeating changes nothing more than doing it once.
    Idempotent,
    /// Repeating it happens again — a second order, a second charge.
    Unsafe,
    /// A method with no defined semantics. Treated as `Unsafe`, because the
    /// alternative is guessing on the user's behalf about their money.
    Unknown,
}

impl Effect {
    /// Does replaying this need the user to say so?
    pub fn needs_confirmation(self) -> bool {
        matches!(self, Effect::Unsafe | Effect::Unknown)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Read => "read",
            Effect::Idempotent => "idempotent",
            Effect::Unsafe => "unsafe",
            Effect::Unknown => "unknown",
        }
    }

    /// What running it does, in words a person can weigh.
    pub fn consequence(self) -> &'static str {
        match self {
            Effect::Read => "reads only; safe to repeat",
            Effect::Idempotent => "changes state, but repeating it changes nothing further",
            Effect::Unsafe => "performs the action again — a second one",
            Effect::Unknown => "has no defined semantics, so it is treated as unrepeatable",
        }
    }
}

impl RequestSpec {
    /// What replaying this request does: what the author declared, or what the
    /// method implies.
    pub fn effect(&self) -> Effect {
        self.settings
            .effect
            .unwrap_or_else(|| effect_of_method(&self.method))
    }
}

/// Classify a method by RFC 9110 semantics.
///
/// Method alone is what the wire tells us. It is right far more often than not
/// and wrong in one common direction — a `POST` used for a search is safe in
/// practice — which is what `[settings] effect` exists to correct. It never errs
/// the other way: nothing safe-by-method performs an action.
pub fn effect_of_method(method: &str) -> Effect {
    match method.to_ascii_uppercase().as_str() {
        "GET" | "HEAD" | "OPTIONS" | "TRACE" => Effect::Read,
        "PUT" | "DELETE" => Effect::Idempotent,
        "POST" | "PATCH" => Effect::Unsafe,
        _ => Effect::Unknown,
    }
}

#[cfg(test)]
mod effect_tests {
    use super::*;

    #[test]
    fn safe_methods_are_reads() {
        for m in ["GET", "get", "HEAD", "OPTIONS", "TRACE"] {
            assert_eq!(effect_of_method(m), Effect::Read, "{m}");
        }
    }

    /// PUT and DELETE change the world, but doing them twice leaves it where
    /// doing them once did — so a replay is recoverable in a way a second POST
    /// is not.
    #[test]
    fn put_and_delete_are_idempotent_not_safe() {
        assert_eq!(effect_of_method("PUT"), Effect::Idempotent);
        assert_eq!(effect_of_method("DELETE"), Effect::Idempotent);
        assert!(!Effect::Idempotent.needs_confirmation());
    }

    #[test]
    fn post_and_patch_need_confirmation() {
        assert_eq!(effect_of_method("POST"), Effect::Unsafe);
        assert_eq!(effect_of_method("PATCH"), Effect::Unsafe);
        assert!(Effect::Unsafe.needs_confirmation());
    }

    /// The direction of the default matters more than the default itself: an
    /// unrecognised method is assumed dangerous, because being wrong the other
    /// way costs somebody real money.
    #[test]
    fn an_unknown_method_is_treated_as_unrepeatable() {
        for m in ["LINK", "PURGE", "MKCOL", ""] {
            assert_eq!(effect_of_method(m), Effect::Unknown, "{m}");
            assert!(Effect::Unknown.needs_confirmation(), "{m}");
        }
    }

    #[test]
    fn every_effect_says_what_running_it_does() {
        for e in [
            Effect::Read,
            Effect::Idempotent,
            Effect::Unsafe,
            Effect::Unknown,
        ] {
            assert!(!e.consequence().is_empty(), "{e:?}");
            assert!(!e.as_str().is_empty(), "{e:?}");
        }
    }
}

#[cfg(test)]
mod effect_override_tests {
    use super::*;

    fn spec(method: &str) -> RequestSpec {
        RequestSpec::new(method, "https://api.test/x")
    }

    #[test]
    fn method_decides_when_nothing_is_declared() {
        assert_eq!(spec("GET").effect(), Effect::Read);
        assert_eq!(spec("POST").effect(), Effect::Unsafe);
    }

    /// The case that makes the feature liveable: a search implemented as POST.
    /// Without this, every run of it would ask, and a prompt people always
    /// accept is worse than no prompt at all.
    #[test]
    fn a_declared_effect_wins() {
        let mut search = spec("POST");
        search.settings.effect = Some(Effect::Read);
        assert_eq!(search.effect(), Effect::Read);
        assert!(!search.effect().needs_confirmation());
    }

    /// It works in the cautious direction too — a GET that triggers something.
    #[test]
    fn a_declaration_can_also_tighten() {
        let mut trigger = spec("GET");
        trigger.settings.effect = Some(Effect::Unsafe);
        assert!(trigger.effect().needs_confirmation());
    }

    #[test]
    fn the_declaration_round_trips_through_toml() {
        let mut original = spec("POST");
        original.settings.effect = Some(Effect::Read);

        let text = toml::to_string(&original).expect("serialise");
        assert!(text.contains("effect = \"read\""), "{text}");

        let parsed: RequestSpec = toml::from_str(&text).expect("parse");
        assert_eq!(parsed.effect(), Effect::Read);
    }

    /// Absent means absent: a file nobody has edited gains no key.
    #[test]
    fn nothing_is_written_when_nothing_is_declared() {
        let text = toml::to_string(&spec("GET")).expect("serialise");
        assert!(!text.contains("effect"), "{text}");
    }
}
