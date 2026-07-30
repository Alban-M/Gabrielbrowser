//! Field-by-field comparison of two responses.
//!
//! "What changed between these two runs?" is the question a text diff answers
//! badly — key order, whitespace and timestamps drown the one field that
//! actually moved. This compares structure, and reports paths.

use crate::model::FieldMap;
use crate::response::ExecutedResponse;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChangeKind {
    Added { after: String },
    Removed { before: String },
    Changed { before: String, after: String },
}

impl Change {
    fn added(path: impl Into<String>, after: &Value) -> Self {
        Change { path: path.into(), kind: ChangeKind::Added { after: render(after) } }
    }
    fn removed(path: impl Into<String>, before: &Value) -> Self {
        Change { path: path.into(), kind: ChangeKind::Removed { before: render(before) } }
    }
    fn changed(path: impl Into<String>, before: &Value, after: &Value) -> Self {
        Change {
            path: path.into(),
            kind: ChangeKind::Changed { before: render(before), after: render(after) },
        }
    }
}

fn render(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Structural diff of two JSON documents.
pub fn diff_json(before: &Value, after: &Value) -> Vec<Change> {
    let mut changes = Vec::new();
    walk("", before, after, &mut changes);
    changes
}

fn walk(path: &str, before: &Value, after: &Value, out: &mut Vec<Change>) {
    match (before, after) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, a_value) in a {
                let child = join(path, key);
                match b.get(key) {
                    Some(b_value) => walk(&child, a_value, b_value, out),
                    None => out.push(Change::removed(child, a_value)),
                }
            }
            for (key, b_value) in b {
                if !a.contains_key(key) {
                    out.push(Change::added(join(path, key), b_value));
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for (i, a_value) in a.iter().enumerate() {
                let child = format!("{path}[{i}]");
                match b.get(i) {
                    Some(b_value) => walk(&child, a_value, b_value, out),
                    None => out.push(Change::removed(child, a_value)),
                }
            }
            for (i, b_value) in b.iter().enumerate().skip(a.len()) {
                out.push(Change::added(format!("{path}[{i}]"), b_value));
            }
        }
        (a, b) if a == b => {}
        (a, b) => out.push(Change::changed(if path.is_empty() { "$" } else { path }, a, b)),
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// The parts of a response worth comparing between runs.
#[derive(Debug, Default)]
pub struct ResponseDiff {
    pub status: Option<(u16, u16)>,
    pub headers: Vec<Change>,
    pub body: Vec<Change>,
    /// Set when at least one body isn't JSON, so a structural diff was impossible.
    pub body_is_opaque: bool,
    pub duration_ms: (u64, u64),
}

impl ResponseDiff {
    pub fn is_empty(&self) -> bool {
        self.status.is_none() && self.headers.is_empty() && self.body.is_empty()
    }
}

/// Headers that differ on every single request and mean nothing to a diff.
const VOLATILE_HEADERS: &[&str] = &[
    "date",
    "age",
    "expires",
    "set-cookie",
    "content-length",
    "etag",
    "last-modified",
    "x-request-id",
    "x-correlation-id",
    "x-amzn-requestid",
    "cf-ray",
    "server-timing",
    "report-to",
    "keep-alive",
];

pub fn diff_responses(before: &ExecutedResponse, after: &ExecutedResponse) -> ResponseDiff {
    let mut diff = ResponseDiff {
        duration_ms: (before.timings.total_ms, after.timings.total_ms),
        ..Default::default()
    };

    if before.status != after.status {
        diff.status = Some((before.status, after.status));
    }

    diff.headers = diff_headers(&before.headers, &after.headers);

    match (before.json(), after.json()) {
        (Some(a), Some(b)) => diff.body = diff_json(&a, &b),
        _ => {
            diff.body_is_opaque = true;
            if before.body != after.body {
                diff.body.push(Change {
                    path: "body".to_string(),
                    kind: ChangeKind::Changed {
                        before: format!("{} bytes", before.body.len()),
                        after: format!("{} bytes", after.body.len()),
                    },
                });
            }
        }
    }

    diff
}

fn diff_headers(before: &FieldMap, after: &FieldMap) -> Vec<Change> {
    let mut changes = Vec::new();
    let interesting = |key: &str| !VOLATILE_HEADERS.contains(&key.to_ascii_lowercase().as_str());

    for (key, value) in before.iter_pairs() {
        if !interesting(key) {
            continue;
        }
        match after.get_first(key) {
            Some(other) if other == value => {}
            Some(other) => changes.push(Change {
                path: key.to_string(),
                kind: ChangeKind::Changed { before: value.to_string(), after: other.to_string() },
            }),
            None => changes.push(Change {
                path: key.to_string(),
                kind: ChangeKind::Removed { before: value.to_string() },
            }),
        }
    }
    for (key, value) in after.iter_pairs() {
        if interesting(key) && !before.contains_key(key) {
            changes.push(Change {
                path: key.to_string(),
                kind: ChangeKind::Added { after: value.to_string() },
            });
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    changes.dedup();
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::Timings;
    use serde_json::json;

    #[test]
    fn reports_changed_added_and_removed_fields() {
        let before = json!({ "id": 1, "name": "ada", "gone": true });
        let after = json!({ "id": 2, "name": "ada", "new": 1 });
        let changes = diff_json(&before, &after);

        assert!(changes.contains(&Change {
            path: "id".into(),
            kind: ChangeKind::Changed { before: "1".into(), after: "2".into() }
        }));
        assert!(changes.iter().any(|c| c.path == "gone" && matches!(c.kind, ChangeKind::Removed { .. })));
        assert!(changes.iter().any(|c| c.path == "new" && matches!(c.kind, ChangeKind::Added { .. })));
        assert!(!changes.iter().any(|c| c.path == "name"), "unchanged field reported");
    }

    #[test]
    fn key_order_is_not_a_change() {
        let a: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        assert!(diff_json(&a, &b).is_empty());
    }

    #[test]
    fn walks_into_nested_arrays() {
        let before = json!({ "items": [{ "id": 1 }, { "id": 2 }] });
        let after = json!({ "items": [{ "id": 1 }, { "id": 99 }, { "id": 3 }] });
        let changes = diff_json(&before, &after);
        assert!(changes.iter().any(|c| c.path == "items[1].id"));
        assert!(changes.iter().any(|c| c.path == "items[2]" && matches!(c.kind, ChangeKind::Added { .. })));
    }

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> ExecutedResponse {
        ExecutedResponse {
            status,
            status_text: String::new(),
            http_version: "HTTP/1.1".into(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: body.as_bytes().to_vec(),
            timings: Timings { ttfb_ms: 1, total_ms: 2 },
            final_url: "https://api.test".into(),
        }
    }

    #[test]
    fn response_diff_ignores_volatile_headers() {
        let a = response(200, &[("Date", "Mon"), ("Content-Type", "application/json")], "{}");
        let b = response(200, &[("Date", "Tue"), ("Content-Type", "application/json")], "{}");
        let diff = diff_responses(&a, &b);
        assert!(diff.is_empty(), "volatile header reported: {:?}", diff.headers);
    }

    #[test]
    fn response_diff_catches_status_and_body() {
        let a = response(200, &[("Content-Type", "application/json")], r#"{"ok":true}"#);
        let b = response(500, &[("Content-Type", "application/json")], r#"{"ok":false}"#);
        let diff = diff_responses(&a, &b);
        assert_eq!(diff.status, Some((200, 500)));
        assert_eq!(diff.body.len(), 1);
        assert_eq!(diff.body[0].path, "ok");
    }

    #[test]
    fn non_json_bodies_fall_back_to_a_size_comparison() {
        let a = response(200, &[("Content-Type", "text/html")], "<p>a</p>");
        let b = response(200, &[("Content-Type", "text/html")], "<p>bb</p>");
        let diff = diff_responses(&a, &b);
        assert!(diff.body_is_opaque);
        assert_eq!(diff.body.len(), 1);
    }
}
