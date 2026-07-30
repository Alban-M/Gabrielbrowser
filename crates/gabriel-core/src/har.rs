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
    #[serde(default)]
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
    #[serde(default = "unknown_http_version", rename = "httpVersion")]
    pub http_version: String,
    #[serde(default)]
    pub cookies: Vec<Cookie>,
    #[serde(default)]
    pub headers: Vec<NameValue>,
    #[serde(default, rename = "queryString")]
    pub query_string: Vec<NameValue>,
    #[serde(default, rename = "postData", skip_serializing_if = "Option::is_none")]
    pub post_data: Option<PostData>,
    #[serde(default = "minus_one", rename = "headersSize")]
    pub headers_size: i64,
    #[serde(default = "minus_one", rename = "bodySize")]
    pub body_size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub status: u16,
    #[serde(default, rename = "statusText")]
    pub status_text: String,
    #[serde(default = "unknown_http_version", rename = "httpVersion")]
    pub http_version: String,
    #[serde(default)]
    pub cookies: Vec<Cookie>,
    #[serde(default)]
    pub headers: Vec<NameValue>,
    #[serde(default)]
    pub content: Content,
    #[serde(default, rename = "redirectURL")]
    pub redirect_url: String,
    #[serde(default = "minus_one", rename = "headersSize")]
    pub headers_size: i64,
    #[serde(default = "minus_one", rename = "bodySize")]
    pub body_size: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Content {
    #[serde(default)]
    pub size: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<i64>,
    #[serde(default, rename = "mimeType")]
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
    #[serde(default, rename = "mimeType")]
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

    for (index, entry) in har.log.entries.iter().enumerate() {
        match capture_from_entry(entry, &format!("{id_prefix}{index:04x}")) {
            Some(capture) => captures.push(capture),
            None => skipped += 1,
        }
    }
    (captures, skipped)
}

fn capture_from_entry(entry: &Entry, id: &str) -> Option<Capture> {
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
        at: crate::parse_iso8601(&entry.started_date_time).unwrap_or_else(crate::now_ms),
        duration_ms: if entry.time.is_finite() && entry.time > 0.0 {
            entry.time as u64
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
mod tests {
    use super::*;

    fn capture(id: &str) -> Capture {
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
