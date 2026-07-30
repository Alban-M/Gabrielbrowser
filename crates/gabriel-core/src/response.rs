use crate::model::FieldMap;
use serde::{Deserialize, Serialize};

/// The result of actually sending a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutedResponse {
    pub status: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub http_version: String,
    pub headers: FieldMap,
    /// Raw bytes, decompressed. Kept as bytes because not every response is text.
    #[serde(with = "crate::b64_bytes")]
    pub body: Vec<u8>,
    pub timings: Timings,
    /// URL after redirects.
    pub final_url: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Timings {
    /// Time until response headers arrived.
    pub ttfb_ms: u64,
    /// Time until the body was fully read.
    pub total_ms: u64,
}

impl ExecutedResponse {
    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn content_type(&self) -> Option<&str> {
        self.headers.get_first("content-type")
    }

    pub fn is_json(&self) -> bool {
        self.content_type()
            .map(|ct| ct.contains("json") || ct.contains("+json"))
            .unwrap_or(false)
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn size(&self) -> usize {
        self.body.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FieldMap;

    fn response(status: u16, content_type: Option<&str>, body: &str) -> ExecutedResponse {
        let mut headers = FieldMap::default();
        if let Some(ct) = content_type {
            headers.set("Content-Type", ct);
        }
        ExecutedResponse {
            status,
            status_text: String::new(),
            http_version: "HTTP/1.1".into(),
            headers,
            body: body.as_bytes().to_vec(),
            timings: Timings::default(),
            final_url: "https://api.test".into(),
        }
    }

    #[test]
    fn json_is_recognised_including_suffixed_types() {
        assert!(response(200, Some("application/json"), "{}").is_json());
        assert!(response(200, Some("application/json; charset=utf-8"), "{}").is_json());
        // `+json` types are JSON too — a problem report is the common one.
        assert!(response(200, Some("application/problem+json"), "{}").is_json());
        assert!(!response(200, Some("text/html"), "<p>").is_json());
        assert!(!response(200, None, "{}").is_json());
    }

    #[test]
    fn success_is_the_two_hundreds_only() {
        for status in [200u16, 201, 204, 299] {
            assert!(
                response(status, None, "").is_success(),
                "{status} should be success"
            );
        }
        for status in [199u16, 301, 400, 500] {
            assert!(
                !response(status, None, "").is_success(),
                "{status} should not be"
            );
        }
    }

    #[test]
    fn the_content_type_is_reported_verbatim() {
        assert_eq!(
            response(200, Some("application/json; charset=utf-8"), "{}").content_type(),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(response(200, None, "").content_type(), None);
    }

    #[test]
    fn a_body_that_is_not_utf8_still_yields_text_lossily() {
        let mut r = response(200, None, "");
        r.body = vec![0xff, 0xfe, b'o', b'k'];
        assert!(r.text().contains("ok"));
        assert_eq!(r.size(), 4);
    }
}
