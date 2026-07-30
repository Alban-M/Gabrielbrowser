//! Captured traffic, and the promotion of a capture into an editable request.
//!
//! This is the seam the whole product is built around: a request the browser
//! already made becomes a request you can edit and replay, *carrying the live
//! session*, without a HAR export or a copied token.

use crate::model::{Auth, Body, FieldMap, Origin, RequestSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One request/response pair observed by the proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capture {
    pub id: String,
    /// Epoch milliseconds.
    pub at: u64,
    pub duration_ms: u64,
    /// Session (Space) this belonged to — the key under which cookies are held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The page that triggered the request, from `Referer` or the tunnel host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    pub request: CapturedRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<CapturedResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedRequest {
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub http_version: String,
    pub headers: FieldMap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<CapturedBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedResponse {
    pub status: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_text: String,
    pub headers: FieldMap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<CapturedBody>,
}

/// A body kept as text when it is text, and base64 when it isn't, so that a
/// capture file stays readable for the 95% case that is JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case")]
pub enum CapturedBody {
    Text { text: String },
    Base64 { data: String },
}

impl CapturedBody {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }
        Some(match std::str::from_utf8(bytes) {
            Ok(text) => CapturedBody::Text { text: text.to_string() },
            Err(_) => CapturedBody::Base64 { data: crate::b64_encode(bytes) },
        })
    }

    pub fn bytes(&self) -> Vec<u8> {
        match self {
            CapturedBody::Text { text } => text.as_bytes().to_vec(),
            CapturedBody::Base64 { data } => crate::b64_decode(data).unwrap_or_default(),
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            CapturedBody::Text { text } => Some(text),
            CapturedBody::Base64 { .. } => None,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            CapturedBody::Text { text } => text.len(),
            CapturedBody::Base64 { data } => data.len() / 4 * 3,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Headers that describe *this* hop rather than the request's intent. Promoting
/// them into a saved request produces subtly broken replays (a stale
/// `Content-Length`, a `Host` pinned to yesterday's tunnel), so they are dropped.
const HOP_BY_HOP: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "accept-encoding",
];

#[derive(Debug, Clone)]
pub struct PromoteOptions {
    /// Inline the captured `Cookie` header into the saved file. Off by default:
    /// collections are meant to be committed, and cookies are credentials.
    pub inline_cookies: bool,
    /// Inline a captured bearer token instead of moving it to the vault. Also
    /// off by default, for the same reason.
    pub inline_token: bool,
    /// Replace the scheme+host with this variable reference, e.g. `{{base_url}}`.
    pub base_url_var: Option<String>,
}

impl Default for PromoteOptions {
    fn default() -> Self {
        Self {
            inline_cookies: false,
            inline_token: false,
            base_url_var: Some("base_url".to_string()),
        }
    }
}

/// The result of promoting a capture: a request to save, plus the credentials
/// that were deliberately *not* written into it.
#[derive(Debug, Clone)]
pub struct Promotion {
    pub spec: RequestSpec,
    /// Suggested vault entries (`name`, `value`) extracted from the capture.
    pub secrets: Vec<(String, String)>,
    /// Suggested environment variables (`name`, `value`), e.g. the base URL.
    pub vars: Vec<(String, String)>,
    /// Session the replay should inherit cookies from.
    pub session: Option<String>,
}

impl Capture {
    /// Turn observed traffic into an editable, committable request.
    pub fn promote(&self, opts: &PromoteOptions) -> Promotion {
        let mut secrets = Vec::new();
        let mut vars = Vec::new();

        let (url, query) = split_query(&self.request.url);
        let (url, base_var) = match (&opts.base_url_var, split_origin(&url)) {
            (Some(var), Some((origin, path))) => {
                vars.push((var.clone(), origin));
                (format!("{{{{{var}}}}}{path}"), true)
            }
            _ => (url, false),
        };
        let _ = base_var;

        let mut spec = RequestSpec::new(&self.request.method, url);
        spec.name = Some(suggest_name(&self.request.method, &self.request.url));
        spec.query = query;

        let mut auth = None;
        for (key, value) in self.request.headers.iter_pairs() {
            let lower = key.to_ascii_lowercase();
            if HOP_BY_HOP.contains(&lower.as_str()) {
                continue;
            }
            match lower.as_str() {
                "cookie" if !opts.inline_cookies => {
                    // The cookies stay in the session store; the file refers to it.
                    auth.get_or_insert(Auth::Session { session: self.session.clone() });
                }
                "authorization" => {
                    if let Some(token) = value.strip_prefix("Bearer ").map(str::trim) {
                        if opts.inline_token {
                            auth = Some(Auth::Bearer { token: token.to_string() });
                        } else {
                            let name = suggest_secret_name(&self.request.url);
                            secrets.push((name.clone(), token.to_string()));
                            auth = Some(Auth::Bearer { token: format!("{{{{secret:{name}}}}}") });
                        }
                    } else if opts.inline_token {
                        spec.headers.insert(key, value);
                    } else {
                        let name = suggest_secret_name(&self.request.url);
                        secrets.push((name.clone(), value.to_string()));
                        spec.headers.insert(key, format!("{{{{secret:{name}}}}}"));
                    }
                }
                _ => spec.headers.insert(key, value),
            }
        }
        spec.auth = auth;

        spec.body = self.request.body.as_ref().map(|body| {
            let content_type = self.request.headers.get_first("content-type").unwrap_or("");
            body_from_capture(body, content_type)
        });

        spec.origin = Some(Origin {
            capture: self.id.clone(),
            promoted_at: crate::now_ms(),
            page: self.page.clone(),
        });

        Promotion { spec, secrets, vars, session: self.session.clone() }
    }

    pub fn status(&self) -> Option<u16> {
        self.response.as_ref().map(|r| r.status)
    }

    /// Host portion of the request URL, for filtering.
    pub fn host(&self) -> &str {
        let rest = self
            .request
            .url
            .split_once("://")
            .map(|(_, r)| r)
            .unwrap_or(&self.request.url);
        rest.split(['/', '?', '#']).next().unwrap_or(rest)
    }
}

fn body_from_capture(body: &CapturedBody, content_type: &str) -> Body {
    let ct = content_type.to_ascii_lowercase();
    match body.as_text() {
        Some(text) if ct.contains("json") => Body::Json { content: text.to_string() },
        Some(text) if ct.contains("x-www-form-urlencoded") => {
            let fields: BTreeMap<String, String> = text
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|pair| {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    (percent_decode(k), percent_decode(v))
                })
                .collect();
            Body::Form { fields }
        }
        Some(text) if ct.contains("graphql") => Body::Json { content: text.to_string() },
        Some(text) => Body::Text {
            content: text.to_string(),
            content_type: (!ct.is_empty()).then(|| content_type.to_string()),
        },
        None => Body::Text {
            content: String::new(),
            content_type: Some(content_type.to_string()),
        },
    }
}

fn split_query(url: &str) -> (String, FieldMap) {
    let Some((base, query)) = url.split_once('?') else {
        return (url.to_string(), FieldMap::default());
    };
    let (query, fragment) = match query.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None => (query, None),
    };
    let mut map = FieldMap::default();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(k), percent_decode(v));
    }
    let base = match fragment {
        Some(f) => format!("{base}#{f}"),
        None => base.to_string(),
    };
    (base, map)
}

/// Split `https://api.test/v1/users` into (`https://api.test`, `/v1/users`).
fn split_origin(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find('/').unwrap_or(rest.len());
    let (host, path) = rest.split_at(end);
    Some((format!("{scheme}://{host}"), path.to_string()))
}

fn suggest_name(method: &str, url: &str) -> String {
    let path = split_origin(url)
        .map(|(_, p)| p)
        .unwrap_or_else(|| url.to_string());
    let path = path.split('?').next().unwrap_or(&path);
    let tail: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let label = if tail.is_empty() {
        "root".to_string()
    } else {
        tail[tail.len().saturating_sub(2)..].join("/")
    };
    format!("{} {}", method.to_uppercase(), label)
}

fn suggest_secret_name(url: &str) -> String {
    let host = split_origin(url)
        .map(|(o, _)| o)
        .unwrap_or_else(|| url.to_string());
    let host = host.trim_start_matches("https://").trim_start_matches("http://");
    let slug: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    format!("{}_token", slug.trim_matches('_'))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&input[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture_with(headers: &[(&str, &str)], body: Option<&str>) -> Capture {
        Capture {
            id: "cap_1".into(),
            at: 1_750_000_000_000,
            duration_ms: 42,
            session: Some("work".into()),
            page: Some("https://app.test/dashboard".into()),
            request: CapturedRequest {
                method: "POST".into(),
                url: "https://api.test/v1/users?page=2&q=ada%20l".into(),
                http_version: "HTTP/2".into(),
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                body: body.map(|b| CapturedBody::Text { text: b.to_string() }),
            },
            response: None,
        }
    }

    #[test]
    fn promotion_moves_cookies_out_of_the_file_and_into_the_session() {
        let capture = capture_with(&[("Cookie", "sid=abc123"), ("Accept", "application/json")], None);
        let promotion = capture.promote(&PromoteOptions::default());

        let text = toml::to_string_pretty(&promotion.spec).unwrap();
        assert!(!text.contains("abc123"), "cookie leaked into the file:\n{text}");
        assert_eq!(
            promotion.spec.auth,
            Some(Auth::Session { session: Some("work".into()) })
        );
        assert_eq!(promotion.session.as_deref(), Some("work"));
    }

    #[test]
    fn promotion_moves_a_bearer_token_into_the_vault() {
        let capture = capture_with(&[("Authorization", "Bearer sk-live-xyz")], None);
        let promotion = capture.promote(&PromoteOptions::default());

        assert_eq!(promotion.secrets, vec![("api_test_token".to_string(), "sk-live-xyz".to_string())]);
        assert_eq!(
            promotion.spec.auth,
            Some(Auth::Bearer { token: "{{secret:api_test_token}}".into() })
        );
        let text = toml::to_string_pretty(&promotion.spec).unwrap();
        assert!(!text.contains("sk-live-xyz"), "token leaked into the file:\n{text}");
    }

    #[test]
    fn promotion_can_inline_credentials_when_asked() {
        let capture = capture_with(&[("Authorization", "Bearer sk-live-xyz")], None);
        let opts = PromoteOptions { inline_token: true, ..Default::default() };
        let promotion = capture.promote(&opts);
        assert_eq!(
            promotion.spec.auth,
            Some(Auth::Bearer { token: "sk-live-xyz".into() })
        );
        assert!(promotion.secrets.is_empty());
    }

    #[test]
    fn promotion_parameterises_the_origin_and_decodes_the_query() {
        let capture = capture_with(&[], None);
        let promotion = capture.promote(&PromoteOptions::default());

        assert_eq!(promotion.spec.url, "{{base_url}}/v1/users");
        assert_eq!(
            promotion.vars,
            vec![("base_url".to_string(), "https://api.test".to_string())]
        );
        assert_eq!(promotion.spec.query.get_first("page"), Some("2"));
        assert_eq!(promotion.spec.query.get_first("q"), Some("ada l"));
    }

    #[test]
    fn promotion_drops_hop_by_hop_headers() {
        let capture = capture_with(
            &[("Host", "api.test"), ("Content-Length", "17"), ("Accept", "*/*")],
            None,
        );
        let spec = capture.promote(&PromoteOptions::default()).spec;
        assert!(!spec.headers.contains_key("host"));
        assert!(!spec.headers.contains_key("content-length"));
        assert!(spec.headers.contains_key("accept"));
    }

    #[test]
    fn json_bodies_are_promoted_as_json() {
        let capture = capture_with(&[("Content-Type", "application/json")], Some(r#"{"a":1}"#));
        let spec = capture.promote(&PromoteOptions::default()).spec;
        assert_eq!(spec.body, Some(Body::Json { content: r#"{"a":1}"#.into() }));
    }

    #[test]
    fn form_bodies_are_promoted_as_fields() {
        let capture = capture_with(
            &[("Content-Type", "application/x-www-form-urlencoded")],
            Some("name=ada+l&role=eng"),
        );
        let spec = capture.promote(&PromoteOptions::default()).spec;
        let Some(Body::Form { fields }) = spec.body else {
            panic!("expected a form body, got {:?}", spec.body);
        };
        assert_eq!(fields.get("name").map(String::as_str), Some("ada l"));
        assert_eq!(fields.get("role").map(String::as_str), Some("eng"));
    }

    #[test]
    fn promotion_records_where_it_came_from() {
        let capture = capture_with(&[], None);
        let spec = capture.promote(&PromoteOptions::default()).spec;
        let origin = spec.origin.expect("origin recorded");
        assert_eq!(origin.capture, "cap_1");
        assert_eq!(origin.page.as_deref(), Some("https://app.test/dashboard"));
    }

    #[test]
    fn binary_bodies_round_trip_through_base64() {
        let bytes = vec![0u8, 159, 146, 150];
        let body = CapturedBody::from_bytes(&bytes).unwrap();
        assert!(matches!(body, CapturedBody::Base64 { .. }));
        assert_eq!(body.bytes(), bytes);
    }
}
