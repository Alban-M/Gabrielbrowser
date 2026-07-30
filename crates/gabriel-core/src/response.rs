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
