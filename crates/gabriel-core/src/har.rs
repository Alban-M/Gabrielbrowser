//! HAR 1.2 — the interchange format for captured traffic.
//!
//! This is the road in and out. In: a developer already has a HAR from Chrome
//! DevTools, Charles, Proxyman or Firefox, and should be able to promote and
//! replay requests from it without re-recording anything. Out: their captures
//! belong to them, and a format every other tool reads is what makes that true
//! rather than a claim.
//!
//! Two decisions worth stating. Real HAR files violate the spec constantly —
//! `headersSize: -1`, absent `queryString`, `pages` missing entirely, timings of
//! `-1` — so every field the spec marks required is treated as optional on the
//! way in, and only `request.url` and `request.method` are genuinely needed.
//! And on the way out, fields Gabriel cannot know are emitted as `-1`, which is
//! what the spec says to do, rather than as plausible-looking zeros.

use crate::capture::{Capture, CapturedBody, CapturedRequest, CapturedResponse};
use crate::model::FieldMap;
use serde::{Deserialize, Serialize};

pub const HAR_VERSION: &str = "1.2";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Har {
    pub log: Log,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Log {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub creator: Creator,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<Page>,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

fn default_version() -> String {
    HAR_VERSION.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Creator {
    pub name: String,
    pub version: String,
}

impl Default for Creator {
    fn default() -> Self {
        Creator {
            name: "gabriel".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub id: String,
    #[serde(rename = "startedDateTime")]
    pub started_date_time: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub page_timings: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default, rename = "pageref", skip_serializing_if = "Option::is_none")]
    pub page_ref: Option<String>,
    #[serde(rename = "startedDateTime")]
    pub started_date_time: String,
    /// Total elapsed milliseconds. `-1` means unknown.
    #[serde(default, deserialize_with = "null_to_default")]
    pub time: f64,
    pub request: Request,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Response>,
    #[serde(default)]
    pub cache: serde_json::Value,
    #[serde(default)]
    pub timings: Timings,
    #[serde(
        default,
        rename = "serverIPAddress",
        skip_serializing_if = "Option::is_none"
    )]
    pub server_ip_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// Where the request came from, when the exporter recorded it.
    #[serde(default, rename = "_page", skip_serializing_if = "Option::is_none")]
    pub gabriel_page: Option<String>,
    /// Gabriel's own session name. An underscore prefix is the spec's mechanism
    /// for custom fields, so other tools will ignore it.
    #[serde(default, rename = "_session", skip_serializing_if = "Option::is_none")]
    pub gabriel_session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    pub url: String,
    #[serde(
        default = "unknown_http_version",
        rename = "httpVersion",
        deserialize_with = "null_to_unknown_version"
    )]
    pub http_version: String,
    #[serde(default)]
    pub cookies: Vec<Cookie>,
    #[serde(default)]
    pub headers: Vec<NameValue>,
    #[serde(default, rename = "queryString")]
    pub query_string: Vec<NameValue>,
    #[serde(default, rename = "postData", skip_serializing_if = "Option::is_none")]
    pub post_data: Option<PostData>,
    #[serde(
        default = "minus_one",
        rename = "headersSize",
        deserialize_with = "null_to_default"
    )]
    pub headers_size: i64,
    #[serde(
        default = "minus_one",
        rename = "bodySize",
        deserialize_with = "null_to_default"
    )]
    pub body_size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(default, deserialize_with = "null_to_default")]
    pub status: u16,
    #[serde(default, rename = "statusText", deserialize_with = "null_to_default")]
    pub status_text: String,
    #[serde(
        default = "unknown_http_version",
        rename = "httpVersion",
        deserialize_with = "null_to_unknown_version"
    )]
    pub http_version: String,
    #[serde(default)]
    pub cookies: Vec<Cookie>,
    #[serde(default)]
    pub headers: Vec<NameValue>,
    #[serde(default)]
    pub content: Content,
    #[serde(default, rename = "redirectURL")]
    pub redirect_url: String,
    #[serde(
        default = "minus_one",
        rename = "headersSize",
        deserialize_with = "null_to_default"
    )]
    pub headers_size: i64,
    #[serde(
        default = "minus_one",
        rename = "bodySize",
        deserialize_with = "null_to_default"
    )]
    pub body_size: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Content {
    #[serde(default, deserialize_with = "null_to_default")]
    pub size: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<i64>,
    #[serde(default, rename = "mimeType", deserialize_with = "null_to_default")]
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `base64` when `text` is encoded rather than literal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NameValue {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostData {
    #[serde(default, rename = "mimeType", deserialize_with = "null_to_default")]
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<NameValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timings {
    #[serde(default = "minus_one")]
    pub blocked: f64,
    #[serde(default = "minus_one")]
    pub dns: f64,
    #[serde(default = "minus_one")]
    pub connect: f64,
    #[serde(default = "minus_one")]
    pub send: f64,
    #[serde(default = "minus_one")]
    pub wait: f64,
    #[serde(default = "minus_one")]
    pub receive: f64,
    #[serde(default = "minus_one")]
    pub ssl: f64,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            blocked: -1.0,
            dns: -1.0,
            connect: -1.0,
            send: -1.0,
            wait: -1.0,
            receive: -1.0,
            ssl: -1.0,
        }
    }
}

fn minus_one<T: From<i8>>() -> T {
    T::from(-1)
}

fn unknown_http_version() -> String {
    "HTTP/1.1".to_string()
}

// ── export ──────────────────────────────────────────────────────────────────

/// Build a HAR from captures, newest last (HAR is chronological).
/// `#[serde(default)]` covers a field that is *absent*. Exporters also write
/// `null` for a value they did not measure, which is a different thing to serde
/// and fails the whole file. This accepts both.
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// As [`null_to_default`], but for a field whose default is not `Default` —
/// an absent HTTP version is "unknown", not the empty string.
fn null_to_unknown_version<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(unknown_http_version))
}

pub fn export(captures: &[Capture]) -> Har {
    let mut entries: Vec<Entry> = captures.iter().map(entry_from_capture).collect();
    entries.sort_by_key(|e| e.started_date_time.clone());
    Har {
        log: Log {
            version: HAR_VERSION.to_string(),
            creator: Creator::default(),
            pages: Vec::new(),
            entries,
        },
    }
}

fn entry_from_capture(capture: &Capture) -> Entry {
    let (_, query) = split_query(&capture.request.url);

    Entry {
        page_ref: None,
        started_date_time: crate::format_iso8601(capture.at),
        time: capture.duration_ms as f64,
        request: Request {
            method: capture.request.method.clone(),
            url: capture.request.url.clone(),
            http_version: normalise_http_version(&capture.request.http_version),
            cookies: cookies_from_header(&capture.request.headers, "cookie"),
            headers: name_values(&capture.request.headers),
            query_string: query,
            post_data: capture.request.body.as_ref().map(|body| PostData {
                mime_type: capture
                    .request
                    .headers
                    .get_first("content-type")
                    .unwrap_or("")
                    .to_string(),
                text: Some(String::from_utf8_lossy(&body.bytes()).into_owned()),
                params: Vec::new(),
            }),
            // Gabriel does not record wire sizes; `-1` is the spec's "unknown".
            headers_size: -1,
            body_size: capture
                .request
                .body
                .as_ref()
                .map(|b| b.len() as i64)
                .unwrap_or(0),
        },
        response: capture.response.as_ref().map(|response| Response {
            status: response.status,
            status_text: response.status_text.clone(),
            http_version: normalise_http_version(&capture.request.http_version),
            cookies: cookies_from_header(&response.headers, "set-cookie"),
            headers: name_values(&response.headers),
            content: match &response.body {
                Some(CapturedBody::Text { text }) => Content {
                    size: text.len() as i64,
                    compression: None,
                    mime_type: response
                        .headers
                        .get_first("content-type")
                        .unwrap_or("")
                        .to_string(),
                    text: Some(text.clone()),
                    encoding: None,
                },
                Some(CapturedBody::Base64 { data }) => Content {
                    size: crate::b64_decode(data)
                        .map(|b| b.len() as i64)
                        .unwrap_or(-1),
                    compression: None,
                    mime_type: response
                        .headers
                        .get_first("content-type")
                        .unwrap_or("")
                        .to_string(),
                    text: Some(data.clone()),
                    encoding: Some("base64".to_string()),
                },
                None => Content {
                    size: 0,
                    compression: None,
                    mime_type: response
                        .headers
                        .get_first("content-type")
                        .unwrap_or("")
                        .to_string(),
                    text: None,
                    encoding: None,
                },
            },
            redirect_url: response
                .headers
                .get_first("location")
                .unwrap_or("")
                .to_string(),
            headers_size: -1,
            body_size: response.body.as_ref().map(|b| b.len() as i64).unwrap_or(0),
        }),
        cache: serde_json::json!({}),
        timings: Timings {
            send: 0.0,
            wait: capture.duration_ms as f64,
            receive: 0.0,
            ..Default::default()
        },
        server_ip_address: None,
        connection: None,
        gabriel_page: capture.page.clone(),
        gabriel_session: capture.session.clone(),
    }
}

// ── import ──────────────────────────────────────────────────────────────────

/// Convert a HAR into captures. Entries that cannot yield a usable request are
/// skipped and counted rather than failing the whole file — a 4 MB HAR with one
/// malformed entry is still worth importing.
pub fn import(har: &Har, id_prefix: &str) -> (Vec<Capture>, usize) {
    let mut captures = Vec::new();
    let mut skipped = 0;

    // An entry whose `startedDateTime` cannot be parsed still has a position in
    // the file, and that position is the only ordering information left. Giving
    // every such entry the same "now" would collapse the sequence; stepping a
    // millisecond per entry keeps it.
    let fallback_base = crate::now_ms();

    for (index, entry) in har.log.entries.iter().enumerate() {
        let fallback_at = fallback_base.saturating_add(index as u64);
        match capture_from_entry(entry, &format!("{id_prefix}{index:04x}"), fallback_at) {
            Some(capture) => captures.push(capture),
            None => skipped += 1,
        }
    }
    (captures, skipped)
}

/// A single exchange lasting longer than this is not something the duration
/// column needs to render faithfully, and a HAR in the wild can carry anything.
/// Without a ceiling, `time: 1e30` casts to `u64::MAX` and prints as
/// "213503982334d 14h".
const MAX_DURATION_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;

fn capture_from_entry(entry: &Entry, id: &str, fallback_at: u64) -> Option<Capture> {
    if entry.request.url.is_empty() || entry.request.method.is_empty() {
        return None;
    }

    let mut request_headers = FieldMap::default();
    for header in &entry.request.headers {
        // HTTP/2 pseudo-headers are not real headers and break replay.
        if header.name.starts_with(':') {
            continue;
        }
        request_headers.insert(&header.name, &header.value);
    }

    let request_body = entry.request.post_data.as_ref().and_then(|data| {
        match &data.text {
            Some(text) if !text.is_empty() => Some(CapturedBody::Text { text: text.clone() }),
            // Some exporters record a form as params with no text.
            _ if !data.params.is_empty() => Some(CapturedBody::Text {
                text: data
                    .params
                    .iter()
                    .map(|p| format!("{}={}", p.name, p.value))
                    .collect::<Vec<_>>()
                    .join("&"),
            }),
            _ => None,
        }
    });

    let response = entry.response.as_ref().map(|response| {
        let mut headers = FieldMap::default();
        for header in &response.headers {
            if header.name.starts_with(':') {
                continue;
            }
            headers.insert(&header.name, &header.value);
        }
        CapturedResponse {
            status: response.status,
            status_text: response.status_text.clone(),
            headers,
            body: body_from_content(&response.content),
        }
    });

    Some(Capture {
        id: id.to_string(),
        at: crate::parse_iso8601(&entry.started_date_time).unwrap_or(fallback_at),
        duration_ms: if entry.time.is_finite() && entry.time > 0.0 {
            entry.time.min(MAX_DURATION_MS) as u64
        } else {
            0
        },
        session: entry.gabriel_session.clone(),
        page: entry.gabriel_page.clone(),
        request: CapturedRequest {
            method: entry.request.method.to_uppercase(),
            url: entry.request.url.clone(),
            http_version: entry.request.http_version.clone(),
            headers: request_headers,
            body: request_body,
        },
        response,
    })
}

fn body_from_content(content: &Content) -> Option<CapturedBody> {
    let text = content.text.as_ref()?;
    if text.is_empty() {
        return None;
    }
    match content.encoding.as_deref() {
        Some("base64") => Some(CapturedBody::Base64 { data: text.clone() }),
        // Anything else (or nothing) means the text is literal.
        _ => Some(CapturedBody::Text { text: text.clone() }),
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn name_values(headers: &FieldMap) -> Vec<NameValue> {
    headers
        .iter_pairs()
        .map(|(name, value)| NameValue {
            name: name.to_string(),
            value: value.to_string(),
        })
        .collect()
}

/// Split `name=value; name2=value2` from a Cookie or Set-Cookie header.
fn cookies_from_header(headers: &FieldMap, header: &str) -> Vec<Cookie> {
    headers
        .iter_pairs()
        .filter(|(name, _)| name.eq_ignore_ascii_case(header))
        .flat_map(|(_, value)| {
            value.split(';').filter_map(|pair| {
                let (name, value) = pair.split_once('=')?;
                let name = name.trim();
                // Set-Cookie attributes (Path, Secure…) are not cookies.
                const ATTRIBUTES: &[&str] = &[
                    "path", "domain", "expires", "max-age", "samesite", "secure", "httponly",
                ];
                (!name.is_empty() && !ATTRIBUTES.contains(&name.to_ascii_lowercase().as_str()))
                    .then(|| Cookie {
                        name: name.to_string(),
                        value: value.trim().to_string(),
                    })
            })
        })
        .collect()
}

fn split_query(url: &str) -> (String, Vec<NameValue>) {
    let Some((base, query)) = url.split_once('?') else {
        return (url.to_string(), Vec::new());
    };
    let query = query.split('#').next().unwrap_or(query);
    let pairs = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            NameValue {
                name: name.to_string(),
                value: value.to_string(),
            }
        })
        .collect();
    (base.to_string(), pairs)
}

/// HAR wants `HTTP/1.1`; Rust's `Version` debug prints `HTTP/1.1` already, but
/// captures can also hold `HTTP/2.0` or an empty string.
fn normalise_http_version(version: &str) -> String {
    match version.trim() {
        "" => unknown_http_version(),
        other => other.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn capture(id: &str) -> Capture {
        let mut request_headers = FieldMap::default();
        request_headers.set("Accept", "application/json");
        request_headers.set("Cookie", "sid=abc123; theme=dark");
        let mut response_headers = FieldMap::default();
        response_headers.set("Content-Type", "application/json");
        response_headers.insert("Set-Cookie", "sid=new; Path=/; HttpOnly");

        Capture {
            id: id.to_string(),
            at: 1_785_283_200_000,
            duration_ms: 42,
            session: Some("work".into()),
            page: Some("https://app.test/dashboard".into()),
            request: CapturedRequest {
                method: "POST".into(),
                url: "https://api.test/v1/orders?page=2&q=hello".into(),
                http_version: "HTTP/2.0".into(),
                headers: request_headers,
                body: Some(CapturedBody::Text {
                    text: r#"{"item":"widget"}"#.into(),
                }),
            },
            response: Some(CapturedResponse {
                status: 201,
                status_text: "Created".into(),
                headers: response_headers,
                body: Some(CapturedBody::Text {
                    text: r#"{"id":7}"#.into(),
                }),
            }),
        }
    }

    #[test]
    fn a_capture_survives_a_round_trip_through_har() {
        let original = capture("cap-1");
        let har = export(std::slice::from_ref(&original));
        let (back, skipped) = import(&har, "har-");

        assert_eq!(skipped, 0);
        assert_eq!(back.len(), 1);
        let restored = &back[0];

        assert_eq!(restored.request.method, original.request.method);
        assert_eq!(restored.request.url, original.request.url);
        assert_eq!(restored.at, original.at);
        assert_eq!(restored.duration_ms, original.duration_ms);
        assert_eq!(restored.session, original.session);
        assert_eq!(restored.page, original.page);
        assert_eq!(
            restored.request.headers.get_first("accept"),
            original.request.headers.get_first("accept")
        );
        assert_eq!(
            restored.request.body.as_ref().map(|b| b.bytes()),
            original.request.body.as_ref().map(|b| b.bytes())
        );
        let (a, b) = (
            restored.response.as_ref().unwrap(),
            original.response.as_ref().unwrap(),
        );
        assert_eq!(a.status, b.status);
        assert_eq!(
            a.body.as_ref().map(|x| x.bytes()),
            b.body.as_ref().map(|x| x.bytes())
        );
    }

    #[test]
    fn the_export_is_valid_json_with_the_expected_shape() {
        let har = export(&[capture("cap-1")]);
        let text = serde_json::to_string_pretty(&har).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(parsed["log"]["version"], "1.2");
        assert_eq!(parsed["log"]["creator"]["name"], "gabriel");
        let entry = &parsed["log"]["entries"][0];
        assert_eq!(entry["request"]["method"], "POST");
        assert_eq!(entry["startedDateTime"], "2026-07-29T00:00:00.000Z");
        assert_eq!(entry["response"]["status"], 201);
        // Unknown sizes are -1, not a made-up number.
        assert_eq!(entry["request"]["headersSize"], -1);
    }

    #[test]
    fn the_query_string_is_broken_out_as_the_spec_requires() {
        let har = export(&[capture("cap-1")]);
        let query = &har.log.entries[0].request.query_string;
        assert_eq!(query.len(), 2);
        assert_eq!(query[0].name, "page");
        assert_eq!(query[0].value, "2");
        assert_eq!(query[1].name, "q");
    }

    #[test]
    fn cookies_are_broken_out_and_attributes_are_not_mistaken_for_cookies() {
        let har = export(&[capture("cap-1")]);
        let entry = &har.log.entries[0];

        let names: Vec<&str> = entry
            .request
            .cookies
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["sid", "theme"]);

        let response_cookies = &entry.response.as_ref().unwrap().cookies;
        assert_eq!(response_cookies.len(), 1, "Path/HttpOnly are not cookies");
        assert_eq!(response_cookies[0].name, "sid");
        assert_eq!(response_cookies[0].value, "new");
    }

    #[test]
    fn a_binary_body_round_trips_as_base64() {
        let mut original = capture("cap-1");
        let bytes = vec![0u8, 159, 146, 150];
        original.response.as_mut().unwrap().body = CapturedBody::from_bytes(&bytes);

        let har = export(std::slice::from_ref(&original));
        assert_eq!(
            har.log.entries[0]
                .response
                .as_ref()
                .unwrap()
                .content
                .encoding
                .as_deref(),
            Some("base64")
        );

        let (back, _) = import(&har, "x-");
        assert_eq!(
            back[0]
                .response
                .as_ref()
                .unwrap()
                .body
                .as_ref()
                .unwrap()
                .bytes(),
            bytes
        );
    }

    /// The shape Chrome DevTools actually produces, including the fields it
    /// leaves at -1 and the pseudo-headers HTTP/2 adds.
    #[test]
    fn a_devtools_style_har_imports() {
        let raw = r#"{
          "log": {
            "version": "1.2",
            "creator": { "name": "WebInspector", "version": "537.36" },
            "pages": [{
              "startedDateTime": "2026-07-29T10:00:00.000Z",
              "id": "page_1",
              "title": "https://app.test/",
              "pageTimings": { "onContentLoad": 120.5, "onLoad": 300.2 }
            }],
            "entries": [{
              "_initiator": { "type": "script" },
              "_priority": "High",
              "_resourceType": "xhr",
              "cache": {},
              "connection": "1234",
              "pageref": "page_1",
              "request": {
                "method": "GET",
                "url": "https://api.test/v2/me",
                "httpVersion": "http/2.0",
                "headers": [
                  { "name": ":method", "value": "GET" },
                  { "name": ":authority", "value": "api.test" },
                  { "name": "accept", "value": "application/json" },
                  { "name": "authorization", "value": "Bearer token-123" }
                ],
                "queryString": [],
                "cookies": [],
                "headersSize": -1,
                "bodySize": 0
              },
              "response": {
                "status": 200,
                "statusText": "",
                "httpVersion": "http/2.0",
                "headers": [
                  { "name": "content-type", "value": "application/json" },
                  { "name": "content-encoding", "value": "gzip" }
                ],
                "cookies": [],
                "content": {
                  "size": 27,
                  "mimeType": "application/json",
                  "text": "{\"id\":\"u_1\",\"name\":\"Ada\"}"
                },
                "redirectURL": "",
                "headersSize": -1,
                "bodySize": -1,
                "_transferSize": 412
              },
              "serverIPAddress": "93.184.216.34",
              "startedDateTime": "2026-07-29T10:00:01.250Z",
              "time": 87.42,
              "timings": {
                "blocked": 1.2, "dns": -1, "ssl": -1, "connect": -1,
                "send": 0.1, "wait": 85.9, "receive": 0.2, "_blocked_queueing": 0.9
              }
            }]
          }
        }"#;

        let har: Har = serde_json::from_str(raw).expect("DevTools HAR should parse");
        let (captures, skipped) = import(&har, "dt-");
        assert_eq!(skipped, 0);
        assert_eq!(captures.len(), 1);

        let capture = &captures[0];
        assert_eq!(capture.request.method, "GET");
        assert_eq!(capture.request.url, "https://api.test/v2/me");
        assert_eq!(capture.duration_ms, 87);
        assert_eq!(capture.at, 1_785_319_201_250);
        // Pseudo-headers are dropped; real ones survive.
        assert!(!capture.request.headers.contains_key(":method"));
        assert_eq!(
            capture.request.headers.get_first("authorization"),
            Some("Bearer token-123")
        );
        assert_eq!(
            capture
                .response
                .as_ref()
                .unwrap()
                .body
                .as_ref()
                .unwrap()
                .as_text(),
            Some(r#"{"id":"u_1","name":"Ada"}"#)
        );
    }

    #[test]
    fn a_minimal_har_with_almost_nothing_in_it_still_imports() {
        let raw = r#"{"log":{"entries":[
          {"startedDateTime":"","request":{"method":"get","url":"http://x.test/"}}
        ]}}"#;
        let har: Har = serde_json::from_str(raw).expect("parse");
        let (captures, skipped) = import(&har, "m-");

        assert_eq!(skipped, 0);
        assert_eq!(
            captures[0].request.method, "GET",
            "method should be normalised"
        );
        assert!(captures[0].response.is_none());
        // An unparseable timestamp falls back to now rather than to 1970.
        assert!(captures[0].at > 1_700_000_000_000);
    }

    #[test]
    fn entries_without_a_usable_request_are_skipped_and_counted() {
        let raw = r#"{"log":{"entries":[
          {"startedDateTime":"2026-07-29T00:00:00Z","request":{"method":"GET","url":""}},
          {"startedDateTime":"2026-07-29T00:00:00Z","request":{"method":"","url":"http://x.test/"}},
          {"startedDateTime":"2026-07-29T00:00:00Z","request":{"method":"GET","url":"http://ok.test/"}}
        ]}}"#;
        let har: Har = serde_json::from_str(raw).expect("parse");
        let (captures, skipped) = import(&har, "s-");
        assert_eq!(captures.len(), 1);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn a_form_recorded_only_as_params_is_reconstructed() {
        let raw = r#"{"log":{"entries":[{
          "startedDateTime":"2026-07-29T00:00:00Z",
          "request":{"method":"POST","url":"http://x.test/login","postData":{
            "mimeType":"application/x-www-form-urlencoded",
            "params":[{"name":"user","value":"ada"},{"name":"pass","value":"s3cret"}]
          }}
        }]}}"#;
        let har: Har = serde_json::from_str(raw).expect("parse");
        let (captures, _) = import(&har, "f-");
        assert_eq!(
            captures[0].request.body.as_ref().unwrap().as_text(),
            Some("user=ada&pass=s3cret")
        );
    }

    #[test]
    fn exported_entries_are_chronological() {
        let mut older = capture("old");
        older.at = 1_000_000_000_000;
        let mut newer = capture("new");
        newer.at = 1_785_283_200_000;

        // Given newest-first, as `capture ls` produces.
        let har = export(&[newer, older]);
        assert!(
            har.log.entries[0].started_date_time < har.log.entries[1].started_date_time,
            "HAR entries must be in time order"
        );
    }

    #[test]
    fn an_empty_capture_list_produces_a_valid_empty_har() {
        let har = export(&[]);
        let text = serde_json::to_string(&har).unwrap();
        assert!(text.contains("\"entries\":[]"));
        let (captures, skipped) = import(&har, "e-");
        assert!(captures.is_empty());
        assert_eq!(skipped, 0);
    }
}

/// HAR export is the one **deliberate exception** to "no secret leaves the
/// process", recorded here so it is a decision rather than an oversight.
///
/// A HAR is a faithful record of traffic — that is the entire point of the
/// format, and what lets DevTools, Charles and Proxyman read Gabriel's exports
/// and Gabriel read theirs. Redacting it would produce a file that no longer
/// describes what happened and cannot be replayed. So `gabriel har export`
/// writes captured credentials, and the safety property is that the user asked
/// for it explicitly and the output goes to a path they named.
#[cfg(test)]
mod har_export_is_a_deliberate_exception {
    use super::tests::capture;
    use super::*;

    #[test]
    fn a_har_export_contains_the_captured_credentials_on_purpose() {
        let har = export(&[capture("c1")]);
        let json = serde_json::to_string(&har).unwrap();

        // If this ever stops being true, the exception has been removed and
        // this test should be deleted along with it — not "fixed".
        assert!(
            json.contains("sid=abc123"),
            "a HAR export that drops cookies cannot be replayed:\n{json}"
        );
    }
}

/// HAR is an interchange format, which means the files Gabriel reads were
/// written by something else — DevTools, Charles, Proxyman, Firefox, a script —
/// each with its own idea of which fields are optional. These are the shapes
/// that actually turn up, and the ones that turned out to matter.
#[cfg(test)]
mod interchange_stability {
    use super::tests::capture;
    use super::*;

    fn entry_json(extra: &str) -> String {
        format!(
            r#"{{
              "startedDateTime": "2026-07-30T10:00:00.000Z",
              "time": 42,
              "request": {{"method": "GET", "url": "https://api.test/x",
                          "httpVersion": "HTTP/1.1", "headers": [], "queryString": [],
                          "cookies": [], "headersSize": -1, "bodySize": -1}},
              "response": {{"status": 200, "statusText": "OK", "httpVersion": "HTTP/1.1",
                           "headers": [], "cookies": [],
                           "content": {{"size": 2, "mimeType": "application/json", "text": "{{}}"}},
                           "redirectURL": "", "headersSize": -1, "bodySize": -1}},
              "cache": {{}}, "timings": {{"send": 0, "wait": 42, "receive": 0}}
              {extra}
            }}"#
        )
    }

    fn har_with(entries: &str) -> Har {
        serde_json::from_str(&format!(
            r#"{{"log": {{"version": "1.2",
                "creator": {{"name": "test", "version": "1"}},
                "entries": [{entries}]}}}}"#
        ))
        .expect("fixture should parse")
    }

    /// `time` is a number in a file somebody else wrote. `1e30` casts to
    /// `u64::MAX`, which rendered as "213503982334d 14h" in `capture ls`.
    #[test]
    fn an_absurd_duration_is_clamped_rather_than_wrapped() {
        let har = har_with(&entry_json(r#", "x": 0"#).replace("\"time\": 42", "\"time\": 1e30"));
        let (captures, _) = import(&har, "t");

        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].duration_ms, MAX_DURATION_MS as u64);
    }

    /// An exporter that did not measure something writes `null`, which is a
    /// different thing to serde than leaving the field out — and used to fail
    /// the whole file.
    #[test]
    fn a_negative_null_or_absent_duration_becomes_zero() {
        for value in ["-1", "null"] {
            let har =
                har_with(&entry_json("").replace("\"time\": 42", &format!("\"time\": {value}")));
            let (captures, _) = import(&har, "t");
            assert_eq!(captures[0].duration_ms, 0, "time: {value}");
        }
    }

    #[test]
    fn nulls_in_place_of_measurements_do_not_fail_the_file() {
        let nulled = entry_json("")
            .replace(r#""statusText": "OK""#, r#""statusText": null"#)
            .replace(r#""httpVersion": "HTTP/1.1""#, r#""httpVersion": null"#)
            .replace(r#""bodySize": -1"#, r#""bodySize": null"#)
            .replace(r#""size": 2"#, r#""size": null"#);

        let (captures, skipped) = import(&har_with(&nulled), "t");
        assert_eq!(skipped, 0);
        assert_eq!(captures.len(), 1);
        // A null version falls back to the same value an absent one does,
        // rather than to an empty string a replay would have to interpret.
        assert!(!captures[0].request.http_version.is_empty());
        assert_eq!(captures[0].request.http_version, unknown_http_version());
    }

    /// Deliberately *not* tolerant: an entry missing `request.url` entirely is
    /// counted as skipped, but a structurally invalid entry fails the import.
    /// Someone importing a HAR is usually looking for one specific request, and
    /// silently dropping malformed entries could drop exactly that one.
    #[test]
    fn a_structurally_invalid_entry_fails_loudly_rather_than_vanishing() {
        let broken = entry_json("").replace(r#""request": {"method""#, r#""request": {"metod""#);
        let parsed: Result<Har, _> = serde_json::from_str(&format!(
            r#"{{"log": {{"version": "1.2",
                "creator": {{"name": "t", "version": "1"}},
                "entries": [{broken}]}}}}"#
        ));
        assert!(parsed.is_err(), "a malformed entry was silently accepted");
    }

    /// The file's order is the only ordering information an entry with an
    /// unreadable date has left. Collapsing them all onto one timestamp loses
    /// which request came first, which is most of what a capture log is for.
    #[test]
    fn entries_with_unreadable_dates_keep_their_relative_order() {
        let broken = entry_json("").replace("2026-07-30T10:00:00.000Z", "not-a-date");
        let har = har_with(&format!("{broken},{broken},{broken}"));

        let (captures, _) = import(&har, "t");
        assert_eq!(captures.len(), 3);
        assert!(
            captures[0].at < captures[1].at && captures[1].at < captures[2].at,
            "order lost: {:?}",
            captures.iter().map(|c| c.at).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_timezone_offset_is_honoured_rather_than_ignored() {
        let utc = har_with(&entry_json(""));
        let offset = har_with(&entry_json("").replace("10:00:00.000Z", "10:00:00.000+02:00"));

        let (utc, _) = import(&utc, "t");
        let (offset, _) = import(&offset, "t");
        // 10:00+02:00 is 08:00Z — two hours earlier, not the same instant.
        assert_eq!(utc[0].at - offset[0].at, 2 * 60 * 60 * 1000);
    }

    /// Firefox omits `statusText`, Charles omits `cache`, and plenty of
    /// exporters omit `pages`. None of that should stop an import.
    #[test]
    fn fields_other_exporters_omit_are_optional() {
        let sparse = r#"{
            "startedDateTime": "2026-07-30T10:00:00.000Z", "time": 1,
            "request": {"method": "get", "url": "https://api.test/x", "headers": []},
            "response": {"status": 204, "headers": [], "content": {}}
        }"#;
        let (captures, skipped) = import(&har_with(sparse), "t");

        assert_eq!(skipped, 0);
        assert_eq!(captures.len(), 1);
        // Method is normalised on the way in, so a replay sends `GET`.
        assert_eq!(captures[0].request.method, "GET");
        assert_eq!(captures[0].response.as_ref().unwrap().status, 204);
    }

    /// The property that makes HAR usable as interchange rather than as a
    /// one-way export: what comes out can go back in and mean the same thing.
    #[test]
    fn import_export_import_is_stable() {
        let first = import(&har_with(&entry_json("")), "t").0;
        let round_tripped = import(&export(&first), "t").0;

        assert_eq!(first.len(), round_tripped.len());
        for (before, after) in first.iter().zip(&round_tripped) {
            assert_eq!(before.request.method, after.request.method);
            assert_eq!(before.request.url, after.request.url);
            assert_eq!(before.at, after.at, "timestamp drifted across a round trip");
            assert_eq!(before.duration_ms, after.duration_ms);
            assert_eq!(
                before.response.as_ref().map(|r| r.status),
                after.response.as_ref().map(|r| r.status)
            );
        }
    }

    /// Exporting what was imported must not lose a header that appeared twice —
    /// `Set-Cookie` is the one that matters, and the one a naive map drops.
    #[test]
    fn a_repeated_header_survives_a_round_trip() {
        let with_cookies = entry_json("").replace(
            r#""headers": [], "cookies": [],
                           "content""#,
            r#""headers": [{"name": "Set-Cookie", "value": "a=1"},
                                       {"name": "Set-Cookie", "value": "b=2"}], "cookies": [],
                           "content""#,
        );
        let imported = import(&har_with(&with_cookies), "t").0;
        let again = import(&export(&imported), "t").0;

        let headers = &again[0].response.as_ref().unwrap().headers;
        let values: Vec<&str> = headers
            .iter_pairs()
            .filter(|(name, _)| name.eq_ignore_ascii_case("Set-Cookie"))
            .map(|(_, value)| value)
            .collect();
        assert_eq!(
            values.len(),
            2,
            "a repeated header was collapsed: {values:?}"
        );
    }

    /// An export of nothing is still a valid HAR, and reimporting it is a
    /// no-op rather than an error.
    #[test]
    fn an_empty_export_reimports_as_empty() {
        let (captures, skipped) = import(&export(&[]), "t");
        assert!(captures.is_empty());
        assert_eq!(skipped, 0);
    }

    /// A capture Gabriel recorded itself is the other direction of the same
    /// property, and the one `gabriel har export` produces.
    #[test]
    fn a_gabriel_capture_survives_export_and_reimport() {
        let original = capture("c1");
        let (round_tripped, skipped) = import(&export(std::slice::from_ref(&original)), "t");

        assert_eq!(skipped, 0);
        let after = &round_tripped[0];
        assert_eq!(after.request.url, original.request.url);
        assert_eq!(after.at, original.at);
        assert_eq!(after.session, original.session);
        assert_eq!(after.page, original.page);
    }
}
